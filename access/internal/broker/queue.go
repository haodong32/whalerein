package broker

// Ordered publisher: one FIFO queue per destination topic, one drain goroutine
// per flow and a bounded global in-flight semaphore. A slow or blocked flow can
// therefore hold at most its own queue and one in-flight slot; small eligible
// messages on another flow keep progressing. Queue depth and waiting bytes are
// both bounded and exhaustion returns a typed error instead of blocking
// silently.

import (
	"context"
	"sync"

	"github.com/whalerein/whalerein/access/internal/config"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
)

type publishFunc func(ctx context.Context, message Message) error

type sendItem struct {
	ctx       context.Context
	message   Message
	size      int64
	done      chan error
	cancelled bool
	started   bool
}

type sendFlow struct {
	key    string
	parent *publisher
	mu     sync.Mutex
	items  []*sendItem
	active bool
	closed bool
}

// publisher owns the bounded ordered submission queues of one MQTT connection.
type publisher struct {
	publish  publishFunc
	metrics  *diagnostics.Metrics
	depth    int
	maxBytes int64
	inFlight chan struct{}
	done     chan struct{}

	mu      sync.Mutex
	flows   map[string]*sendFlow
	waiting int64
	closed  bool
	closeIn sync.Once
}

func newPublisher(publish publishFunc, transport config.TransportEffective, metrics *diagnostics.Metrics) *publisher {
	if publish == nil {
		return nil
	}
	return &publisher{
		publish:  publish,
		metrics:  metrics,
		depth:    transport.QueueDepth(),
		maxBytes: transport.QueueBytes(),
		inFlight: make(chan struct{}, transport.MaxInFlight()),
		done:     make(chan struct{}),
		flows:    map[string]*sendFlow{},
	}
}

func flowLabel(topic string) string {
	parsed, err := ParseTopic(topic)
	if err != nil {
		return "other"
	}
	switch parsed.Kind {
	case KindRequest:
		return "request"
	case KindResult:
		return "result"
	case KindEvent:
		return "event"
	case KindDeviceEvent:
		return "device_event"
	case KindDeviceNotice:
		return "notice"
	case KindDeviceQuery:
		return "query"
	case KindDeviceReply:
		return "reply"
	case KindPresence:
		return "presence"
	default:
		return "other"
	}
}

func (p *publisher) setMetrics(kind string, flow *sendFlow) {
	if p.metrics == nil {
		return
	}
	flow.mu.Lock()
	depth := len(flow.items)
	active := flow.active
	flow.mu.Unlock()
	label := flowLabel(kind)
	p.metrics.TransportQueue(label, depth)
	inflight := 0
	if active {
		inflight = 1
	}
	p.metrics.TransportInFlightCount(label, inflight)
}

// Submit enqueues one payload on one topic. It is the property-free shorthand
// used by internal control and notices.
func (p *publisher) Submit(ctx context.Context, topic string, payload []byte) error {
	return p.SubmitMessage(ctx, Message{Topic: topic, Payload: payload, QoS: 1})
}

// SubmitMessage enqueues one complete prepared packet, including its MQTT 5
// properties, and waits for its publication. It never splits or retries a
// packet.
func (p *publisher) SubmitMessage(ctx context.Context, message Message) error {
	if p == nil {
		return ErrQueueClosed
	}
	if len(message.Payload) == 0 {
		return transportReject(transportCodeHeader, ErrMessageBudget)
	}
	topic := message.Topic
	size := int64(len(message.Payload))
	p.mu.Lock()
	if p.closed {
		p.mu.Unlock()
		return ErrQueueClosed
	}
	flow := p.flows[topic]
	if flow == nil {
		flow = &sendFlow{key: topic, parent: p}
		p.flows[topic] = flow
	}
	flow.mu.Lock()
	if flow.closed {
		flow.mu.Unlock()
		p.mu.Unlock()
		return ErrQueueClosed
	}
	if len(flow.items) >= p.depth {
		flow.mu.Unlock()
		p.mu.Unlock()
		if p.metrics != nil {
			p.metrics.TransportRejected(transportCodeQueue, size, int64(p.depth))
		}
		return ErrQueueFull
	}
	if size > p.maxBytes || p.waiting+size > p.maxBytes {
		flow.mu.Unlock()
		p.mu.Unlock()
		if p.metrics != nil {
			p.metrics.TransportRejected(transportCodeQueue, size, p.maxBytes)
		}
		return ErrQueueBytes
	}
	item := &sendItem{ctx: ctx, message: message, size: size, done: make(chan error, 1)}
	flow.items = append(flow.items, item)
	if !flow.active {
		flow.active = true
		go flow.drain()
	}
	p.waiting += size
	flow.mu.Unlock()
	p.mu.Unlock()
	p.setMetrics(topic, flow)

	select {
	case err := <-item.done:
		return err
	case <-ctx.Done():
		flow.cancel(item)
		return ctx.Err()
	}
}

func (f *sendFlow) cancel(item *sendItem) {
	f.mu.Lock()
	if !item.started {
		item.cancelled = true
	}
	f.mu.Unlock()
}

func (f *sendFlow) reclaim(item *sendItem) {
	p := f.parent
	p.mu.Lock()
	p.waiting -= item.size
	if p.waiting < 0 {
		p.waiting = 0
	}
	p.mu.Unlock()
}

func (f *sendFlow) drain() {
	p := f.parent
	for {
		f.mu.Lock()
		for len(f.items) > 0 && f.items[0].cancelled {
			cancelled := f.items[0]
			f.items = f.items[1:]
			f.mu.Unlock()
			f.reclaim(cancelled)
			cancelled.done <- context.Canceled
			f.mu.Lock()
		}
		if len(f.items) == 0 {
			f.active = false
			f.mu.Unlock()
			p.setMetrics(f.key, f)
			return
		}
		item := f.items[0]
		f.items = f.items[1:]
		item.started = true
		f.mu.Unlock()
		p.setMetrics(f.key, f)

		select {
		case p.inFlight <- struct{}{}:
		case <-p.done:
			f.fail(item, nil)
			continue
		case <-item.ctx.Done():
			f.fail(item, item.ctx.Err())
			continue
		}
		item.message.Topic = f.key
		if err := p.publish(item.ctx, item.message); err != nil {
			if p.metrics != nil {
				p.metrics.TransportRejected(transportCodeQueue, item.size, 0)
			}
			item.done <- err
		} else {
			item.done <- nil
		}
		<-p.inFlight
		item.message.Payload = nil
		f.reclaim(item)
	}
}

// fail drops the head item with err and reclaims its waiting bytes.
func (f *sendFlow) fail(item *sendItem, err error) {
	if err == nil {
		err = ErrQueueClosed
	}
	item.message.Payload = nil
	f.reclaim(item)
	item.done <- err
}

// Close rejects new submissions, releases waiting publishers and lets the
// MQTT connection teardown interrupt in-flight work.
func (p *publisher) Close() {
	if p == nil {
		return
	}
	p.closeIn.Do(func() {
		p.mu.Lock()
		p.closed = true
		flows := make([]*sendFlow, 0, len(p.flows))
		for _, flow := range p.flows {
			flow.mu.Lock()
			flow.closed = true
			flow.mu.Unlock()
			flows = append(flows, flow)
		}
		close(p.done)
		p.mu.Unlock()
		for _, flow := range flows {
			p.setMetrics(flow.key, flow)
		}
	})
}

// WaitingBytes reports the current bounded waiting bytes (operational test
// seam).
func (p *publisher) WaitingBytes() int64 {
	if p == nil {
		return 0
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.waiting
}
