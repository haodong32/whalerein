package broker

import (
	"context"
	"errors"
	"fmt"
	"strconv"
	"strings"
	"sync"
	"time"
)

// Private broker control topics. Only the internal Access identity may write
// the request topic or read the reply and event topics.
const (
	ControlRequestTopic = "$CONTROL/whalerein/v1"
	ControlReplyFilter  = "whalerein/v1/control/reply"
	ControlEventFilter  = "whalerein/v1/control/events"
)

// ControlReply is one acknowledged management operation.
type ControlReply struct {
	ID       string
	OK       bool
	Error    string
	Fields   map[string]string
	Clients  []SnapshotClient
	Sequence uint64
}

// SnapshotClient is one broker connection at the snapshot boundary.
type SnapshotClient struct {
	SessionID string
	ClientID  string
	Kind      string
	HostID    string
	DeviceID  string
	ConnID    uint64
	Revision  uint64
	State     string
	Sequence  uint64
	Connected int64
}

// Notification is one ordered connection lifecycle event.
type Notification struct {
	Sequence    uint64
	Event       string
	ClientID    string
	Kind        string
	HostID      string
	DeviceID    string
	ConnID      uint64
	AtUnixMilli int64
	Reason      int
}

// Control is the authenticated management client for the Mosquitto line. One
// request is outstanding at a time: Access is the only policy writer.
type Control struct {
	mu      sync.Mutex
	publish func(ctx context.Context, topic string, payload []byte) error
	waiters map[string]chan *ControlReply
	nextID  uint64

	eventsMu sync.RWMutex
	onEvent  func(*Notification)

	timeout time.Duration
}

// NewControl builds a management client over the Access MQTT session.
func NewControl(publish func(ctx context.Context, topic string, payload []byte) error) (*Control, error) {
	if publish == nil {
		return nil, errors.New("broker: control publish function is required")
	}
	return &Control{
		publish: publish,
		waiters: map[string]chan *ControlReply{},
		timeout: 5 * time.Second,
	}, nil
}

// SetNotification installs the ordered presence sink.
func (c *Control) SetNotification(sink func(*Notification)) {
	c.eventsMu.Lock()
	c.onEvent = sink
	c.eventsMu.Unlock()
}

// Request publishes one control operation and waits for its acknowledgement.
func (c *Control) Request(ctx context.Context, op string, fields []string) (*ControlReply, error) {
	// Register the waiter, then release the lock before publishing and waiting:
	// the reply arrives on the same session and handleReply needs the lock to
	// find the waiter.
	c.mu.Lock()
	c.nextID++
	id := fmt.Sprintf("access-%d-%d", time.Now().UnixMilli(), c.nextID)
	payload := make([]string, 0, len(fields)+3)
	payload = append(payload, "whalerein/1", "id "+id, "op "+op)
	payload = append(payload, fields...)
	waiter := make(chan *ControlReply, 1)
	c.waiters[id] = waiter
	c.mu.Unlock()
	defer func() {
		c.mu.Lock()
		delete(c.waiters, id)
		c.mu.Unlock()
	}()

	publishCtx, cancel := context.WithTimeout(ctx, c.timeout)
	defer cancel()
	if err := c.publish(publishCtx, ControlRequestTopic, []byte(strings.Join(payload, "\n")+"\n")); err != nil {
		return nil, fmt.Errorf("broker control %s: %w", op, err)
	}
	select {
	case <-ctx.Done():
		return nil, ctx.Err()
	case <-time.After(c.timeout):
		return nil, fmt.Errorf("broker control %s: timeout", op)
	case reply := <-waiter:
		if reply == nil {
			return nil, fmt.Errorf("broker control %s: empty reply", op)
		}
		if !reply.OK {
			return reply, fmt.Errorf("broker control %s: %s", op, reply.Error)
		}
		return reply, nil
	}
}

// HandleMessage consumes a control reply or presence notification. It returns
// true when the message belonged to the control interface.
func (c *Control) HandleMessage(topic string, payload []byte) bool {
	switch topic {
	case ControlReplyFilter:
		c.handleReply(payload)
		return true
	case ControlEventFilter:
		c.handleNotification(payload)
		return true
	default:
		return false
	}
}

func parseControlLines(payload []byte) map[string][]string {
	lines := strings.Split(string(payload), "\n")
	fields := map[string][]string{}
	for index, line := range lines {
		line = strings.TrimSuffix(line, "\r")
		if index == 0 || line == "" {
			continue
		}
		key, value, ok := strings.Cut(line, " ")
		if !ok {
			fields[line] = append(fields[line], "")
			continue
		}
		fields[key] = append(fields[key], value)
	}
	return fields
}

func firstField(fields map[string][]string, key string) string {
	if values, ok := fields[key]; ok && len(values) > 0 {
		return values[0]
	}
	return ""
}

func (c *Control) handleReply(payload []byte) {
	fields := parseControlLines(payload)
	id := firstField(fields, "id")
	reply := &ControlReply{ID: id, Fields: map[string]string{}}
	if _, ok := fields["ok"]; ok {
		reply.OK = true
	} else {
		reply.Error = strings.TrimSpace(strings.Join(fields["error"], " "))
	}
	for key, values := range fields {
		if key == "client" || key == "id" || key == "ok" || key == "error" || key == "whalerein/1" {
			continue
		}
		if len(values) > 0 {
			reply.Fields[key] = values[0]
		}
	}
	if sequence, err := strconv.ParseUint(reply.Fields["seq"], 10, 64); err == nil {
		reply.Sequence = sequence
	}
	for _, line := range fields["client"] {
		parts := strings.Split(line, " ")
		if len(parts) < 9 {
			continue
		}
		client := SnapshotClient{
			ClientID: parts[0],
			Kind:     parts[1],
			HostID:   parts[2],
			DeviceID: parts[3],
			State:    parts[6],
		}
		client.ConnID, _ = strconv.ParseUint(parts[4], 10, 64)
		client.Revision, _ = strconv.ParseUint(parts[5], 10, 64)
		client.Sequence, _ = strconv.ParseUint(parts[7], 10, 64)
		client.Connected, _ = strconv.ParseInt(parts[8], 10, 64)
		if len(parts) > 9 && parts[9] != "-" {
			client.SessionID = parts[9]
		}
		if client.HostID == "-" {
			client.HostID = ""
		}
		if client.DeviceID == "-" {
			client.DeviceID = ""
		}
		reply.Clients = append(reply.Clients, client)
	}
	c.mu.Lock()
	waiter := c.waiters[id]
	c.mu.Unlock()
	if waiter != nil {
		select {
		case waiter <- reply:
		default:
		}
	}
}

func (c *Control) handleNotification(payload []byte) {
	fields := parseControlLines(payload)
	notification := &Notification{
		Event:    firstField(fields, "event"),
		ClientID: firstField(fields, "client"),
		Kind:     firstField(fields, "kind"),
		HostID:   firstField(fields, "host"),
		DeviceID: firstField(fields, "device"),
	}
	notification.Sequence, _ = strconv.ParseUint(firstField(fields, "seq"), 10, 64)
	notification.ConnID, _ = strconv.ParseUint(firstField(fields, "conn"), 10, 64)
	notification.AtUnixMilli, _ = strconv.ParseInt(firstField(fields, "at"), 10, 64)
	reason, _ := strconv.Atoi(firstField(fields, "reason"))
	notification.Reason = reason
	if notification.HostID == "-" {
		notification.HostID = ""
	}
	if notification.DeviceID == "-" {
		notification.DeviceID = ""
	}
	c.eventsMu.RLock()
	sink := c.onEvent
	c.eventsMu.RUnlock()
	if sink != nil && notification.Sequence > 0 {
		sink(notification)
	}
}
