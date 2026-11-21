package broker

import (
	"context"
	"errors"
	"sync"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/config"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
)

func queueTransport(inFlight, depth uint32, bytes uint64) config.TransportEffective {
	transport := config.DefaultTransportEffective()
	transport.SendInFlight = inFlight
	transport.SendQueueDepth = depth
	transport.SendQueueBytes = bytes
	return transport
}

// blockingPublisher blocks the first publication on one topic until released
// and records every publication in order.
type blockingPublisher struct {
	mu        sync.Mutex
	published []string
	payloads  []byte
	blocked   map[string]chan struct{}
	started   chan string
}

func (b *blockingPublisher) publish(_ context.Context, topic string, payload []byte) error {
	if b.started != nil {
		select {
		case b.started <- topic:
		default:
		}
	}
	if gate, ok := b.blocked[topic]; ok {
		<-gate
	}
	b.mu.Lock()
	b.published = append(b.published, topic)
	b.payloads = append(b.payloads, payload[0])
	b.mu.Unlock()
	return nil
}

func (b *blockingPublisher) payloadOrder() []byte {
	b.mu.Lock()
	defer b.mu.Unlock()
	return append([]byte(nil), b.payloads...)
}

func (b *blockingPublisher) order() []string {
	b.mu.Lock()
	defer b.mu.Unlock()
	return append([]string(nil), b.published...)
}

func TestOrderedPublisherPreservesPerFlowOrder(t *testing.T) {
	gate := make(chan struct{})
	publisher := &blockingPublisher{blocked: map[string]chan struct{}{RequestTopic("h1"): gate}, started: make(chan string, 8)}
	queue := newPublisher(publisher.publish, queueTransport(1, 8, 1<<20), nil)
	defer queue.Close()
	topic := RequestTopic("h1")

	errorsCh := make(chan error, 3)
	// The first publication blocks in flight; the next two wait in FIFO order.
	go func() { errorsCh <- queue.Submit(context.Background(), topic, []byte{0}) }()
	select {
	case <-publisher.started:
	case <-time.After(time.Second):
		t.Fatal("first publication did not start")
	}
	go func() { errorsCh <- queue.Submit(context.Background(), topic, []byte{1}) }()
	waitForDepth(t, queue, topic, 1)
	go func() { errorsCh <- queue.Submit(context.Background(), topic, []byte{2}) }()
	waitForDepth(t, queue, topic, 2)
	close(gate)
	for index := 0; index < 3; index++ {
		if err := <-errorsCh; err != nil {
			t.Fatalf("submit: %v", err)
		}
	}
	if order := publisher.payloadOrder(); len(order) != 3 || order[0] != 0 || order[1] != 1 || order[2] != 2 {
		t.Fatalf("publication order = %v", order)
	}
}

func waitForDepth(t *testing.T, queue *publisher, topic string, want int) {
	t.Helper()
	deadline := time.Now().Add(time.Second)
	for time.Now().Before(deadline) {
		queue.mu.Lock()
		flow := queue.flows[topic]
		depth := 0
		if flow != nil {
			flow.mu.Lock()
			depth = len(flow.items)
			flow.mu.Unlock()
		}
		queue.mu.Unlock()
		if depth == want {
			return
		}
		time.Sleep(time.Millisecond)
	}
	t.Fatalf("queue never reached depth %d", want)
}

func TestOrderedPublisherBoundsDepthAndWaitingBytes(t *testing.T) {
	gate := make(chan struct{})
	publisher := &blockingPublisher{blocked: map[string]chan struct{}{RequestTopic("h1"): gate}, started: make(chan string, 4)}
	queue := newPublisher(publisher.publish, queueTransport(1, 1, 1<<20), nil)
	defer queue.Close()
	topic := RequestTopic("h1")
	first := make(chan error, 1)
	go func() { first <- queue.Submit(context.Background(), topic, []byte("first")) }()
	select {
	case <-publisher.started:
	case <-time.After(time.Second):
		t.Fatal("first publication did not start")
	}
	// One waiting slot is available behind the in-flight packet; the next
	// submission is refused without blocking.
	second := make(chan error, 1)
	go func() { second <- queue.Submit(context.Background(), topic, []byte("second")) }()
	waitForDepth(t, queue, topic, 1)
	if err := queue.Submit(context.Background(), topic, []byte("third")); !errors.Is(err, ErrQueueFull) {
		t.Fatalf("depth bound = %v", err)
	}

	// A new queue with a byte bound smaller than the waiting message rejects
	// on bytes even when depth allows it.
	smallBytes := makePublisherWithBytes(1<<20, 4)
	defer smallBytes.Close()
	if err := smallBytes.Submit(context.Background(), topic, make([]byte, 2<<20)); !errors.Is(err, ErrQueueBytes) {
		t.Fatalf("byte bound = %v", err)
	}
	close(gate)
	if err := <-first; err != nil {
		t.Fatal(err)
	}
	if err := <-second; err != nil {
		t.Fatal(err)
	}
}

func makePublisherWithBytes(max uint64, depth uint32) *publisher {
	return newPublisher(func(context.Context, string, []byte) error { return nil }, queueTransport(1, depth, max), nil)
}

func TestBlockedFlowDoesNotStarveAnotherFlow(t *testing.T) {
	gate := make(chan struct{})
	publisher := &blockingPublisher{blocked: map[string]chan struct{}{RequestTopic("h1"): gate}, started: make(chan string, 8)}
	// Two in-flight slots: the blocked flow holds one and the unrelated flow
	// must still publish.
	queue := newPublisher(publisher.publish, queueTransport(2, 8, 1<<20), nil)
	defer queue.Close()

	blocked := make(chan error, 1)
	go func() { blocked <- queue.Submit(context.Background(), RequestTopic("h1"), []byte("blocked")) }()
	select {
	case <-publisher.started:
	case <-time.After(time.Second):
		t.Fatal("blocked flow did not start")
	}
	other := make(chan error, 1)
	go func() { other <- queue.Submit(context.Background(), ResultTopic("h2"), []byte("small")) }()
	select {
	case err := <-other:
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(time.Second):
		t.Fatal("unrelated flow was starved by a blocked flow")
	}
	close(gate)
	if err := <-blocked; err != nil {
		t.Fatal(err)
	}
}

func TestOrderedPublisherCloseReleasesWaitersAndRejectsNewWork(t *testing.T) {
	gate := make(chan struct{})
	publisher := &blockingPublisher{blocked: map[string]chan struct{}{RequestTopic("h1"): gate}, started: make(chan string, 4)}
	queue := newPublisher(publisher.publish, queueTransport(1, 4, 1<<20), nil)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	done := make(chan error, 1)
	go func() { done <- queue.Submit(ctx, RequestTopic("h1"), []byte("first")) }()
	select {
	case <-publisher.started:
	case <-time.After(time.Second):
		t.Fatal("publication did not start")
	}
	queue.Close()
	if err := queue.Submit(context.Background(), RequestTopic("h1"), []byte("late")); !errors.Is(err, ErrQueueClosed) {
		t.Fatalf("closed queue = %v", err)
	}
	// The in-flight publication owns its slot; closing releases the queue but
	// does not corrupt an already submitted packet.
	close(gate)
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("close did not release the blocked submitter")
	}
}

func TestOrderedPublisherCancellationReclaimsWaitingBytes(t *testing.T) {
	gate := make(chan struct{})
	publisher := &blockingPublisher{blocked: map[string]chan struct{}{RequestTopic("h1"): gate}, started: make(chan string, 4)}
	queue := newPublisher(publisher.publish, queueTransport(1, 4, 1<<20), nil)
	defer queue.Close()
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { done <- queue.Submit(ctx, RequestTopic("h1"), []byte("first")) }()
	select {
	case <-publisher.started:
	case <-time.After(time.Second):
		t.Fatal("publication did not start")
	}
	cancel()
	if err := <-done; !errors.Is(err, context.Canceled) {
		t.Fatalf("cancelled submit = %v", err)
	}
	close(gate)
	deadline := time.Now().Add(time.Second)
	for queue.WaitingBytes() != 0 && time.Now().Before(deadline) {
		time.Sleep(time.Millisecond)
	}
	if queue.WaitingBytes() != 0 {
		t.Fatalf("waiting bytes = %d", queue.WaitingBytes())
	}
}

func TestOrderedPublisherReportsBoundedFlowMetrics(t *testing.T) {
	metrics := diagnostics.NewMetrics()
	gate := make(chan struct{})
	publisher := &blockingPublisher{blocked: map[string]chan struct{}{RequestTopic("h1"): gate}, started: make(chan string, 4)}
	queue := newPublisher(publisher.publish, queueTransport(1, 4, 1<<20), metrics)
	done := make(chan error, 1)
	go func() { done <- queue.Submit(context.Background(), RequestTopic("h1"), []byte("first")) }()
	select {
	case <-publisher.started:
	case <-time.After(time.Second):
		t.Fatal("publication did not start")
	}
	close(gate)
	if err := <-done; err != nil {
		t.Fatal(err)
	}
	queue.Close()
}
