package broker

import (
	"context"
	"encoding/json"
	"log/slog"
	"sync"
	"time"

	"github.com/whalerein/whalerein/access/internal/diagnostics"
)

// Presence sources for device lifecycle events. Native broker notifications
// are the primary source; the device last will and HostReady are the existing
// fallbacks when a notification is lost.
const (
	PresenceSourceBroker = "broker"
	PresenceSourceWill   = "will"
)

// DeviceOfflineHandler runs once for each accepted online -> offline edge.
// observedAt is the source timestamp used for stale-event rejection.
type DeviceOfflineHandler func(deviceID, source string, observedAt time.Time)

type presenceEntry struct {
	online bool
	at     time.Time
	connID uint64
}

// Presence is the broker-derived online registry. It is seeded by ordered
// broker connection notifications, a broker snapshot, the device last will or
// a Host Ready signal. It never queries a database heartbeat.
type Presence struct {
	mu                   sync.RWMutex
	hosts                map[string]presenceEntry
	devices              map[string]presenceEntry
	cacheTTL             time.Duration
	available            bool
	lastSeq              uint64
	metrics              *diagnostics.Metrics
	logger               *slog.Logger
	now                  func() time.Time
	deviceOfflineHandler DeviceOfflineHandler
}

// NewPresence creates an empty broker presence cache. Presence stays
// unavailable until the first snapshot establishes the current broker state.
func NewPresence(cacheTTL time.Duration, metrics *diagnostics.Metrics, logger *slog.Logger) *Presence {
	if cacheTTL <= 0 {
		cacheTTL = 15 * time.Second
	}
	if logger == nil {
		logger = slog.Default()
	}
	p := &Presence{
		hosts:    make(map[string]presenceEntry),
		devices:  make(map[string]presenceEntry),
		cacheTTL: cacheTTL,
		metrics:  metrics,
		logger:   logger,
		now:      func() time.Time { return time.Now().UTC() },
	}
	p.publishCounts()
	return p
}

// SetAvailable marks whether broker state is currently established. While
// unavailable, presence reports offline rather than inventing connectivity.
func (p *Presence) SetAvailable(available bool) {
	p.mu.Lock()
	p.available = available
	p.mu.Unlock()
	if !available {
		p.publishCounts()
	}
}

// Available reports whether a snapshot or a live notification established
// broker state since the current Access session started.
func (p *Presence) Available() bool {
	p.mu.RLock()
	defer p.mu.RUnlock()
	return p.available
}

// IsHostOnline returns broker presence. A missing or unavailable presence view
// reports offline.
func (p *Presence) IsHostOnline(ctx context.Context, hostID string) bool {
	if hostID == "" {
		return false
	}
	p.mu.RLock()
	defer p.mu.RUnlock()
	if !p.available {
		return false
	}
	entry, ok := p.hosts[hostID]
	if !ok {
		return false
	}
	return entry.online
}

// SetHostOnline updates broker-derived host presence.
func (p *Presence) SetHostOnline(hostID string, online bool, at time.Time) {
	if hostID == "" {
		return
	}
	p.mu.Lock()
	if previous, exists := p.hosts[hostID]; !exists || !at.Before(previous.at) {
		p.hosts[hostID] = presenceEntry{online: online, at: at, connID: previous.connID}
	}
	p.available = true
	p.mu.Unlock()
	p.publishCounts()
}

// SetDeviceOnline updates broker-derived device presence. An online -> offline
// transition invokes the registered device-offline handler exactly once.
func (p *Presence) SetDeviceOnline(deviceID string, online bool, at time.Time) {
	p.applyDevice(deviceID, online, at, PresenceSourceBroker, 0)
}

// SetDeviceOfflineHandler installs the callback for accepted device-offline
// transitions. The callback runs outside the presence lock.
func (p *Presence) SetDeviceOfflineHandler(handler DeviceOfflineHandler) {
	p.mu.Lock()
	p.deviceOfflineHandler = handler
	p.mu.Unlock()
}

// applyDevice records device presence and emits at most one offline callback
// per online -> offline edge. A non-zero connID limits the transition to its
// own connection so a delayed disconnect cannot affect a newer one.
func (p *Presence) applyDevice(deviceID string, online bool, at time.Time, source string, connID uint64) {
	if deviceID == "" {
		return
	}
	p.mu.Lock()
	previous, exists := p.devices[deviceID]
	if exists && at.Before(previous.at) {
		p.mu.Unlock()
		return
	}
	if connID != 0 && previous.connID != 0 && previous.connID != connID {
		p.mu.Unlock()
		p.logger.Debug("presence event for a superseded connection ignored", "source", source)
		return
	}
	transition := false
	duplicate := false
	if !online {
		if !exists || previous.online {
			transition = true
		} else {
			duplicate = true
		}
	}
	next := presenceEntry{online: online, at: at, connID: previous.connID}
	if connID != 0 {
		next.connID = connID
	}
	p.devices[deviceID] = next
	handler := p.deviceOfflineHandler
	p.mu.Unlock()

	p.publishCounts()
	if duplicate {
		if p.metrics != nil {
			p.metrics.PresenceDuplicate()
		}
		return
	}
	state := "offline"
	if online {
		state = "online"
	}
	if p.metrics != nil {
		p.metrics.PresenceEvent(source, state)
	}
	if transition && handler != nil {
		handler(deviceID, source, at)
	}
}

// MarkHostReady treats a ready host result as broker presence.
func (p *Presence) MarkHostReady(hostID string, at time.Time) {
	p.SetHostOnline(hostID, true, at)
}

// HandleNotification applies one ordered broker connection notification.
func (p *Presence) HandleNotification(notification *Notification) {
	if notification == nil || notification.Sequence == 0 {
		return
	}
	at := p.now()
	if notification.AtUnixMilli > 0 {
		at = time.UnixMilli(notification.AtUnixMilli).UTC()
	}
	p.mu.Lock()
	if notification.Sequence <= p.lastSeq {
		p.mu.Unlock()
		return
	}
	p.lastSeq = notification.Sequence
	online := notification.Event == "connected"
	switch notification.Kind {
	case "host":
		if notification.HostID != "" {
			previous, exists := p.hosts[notification.HostID]
			superseded := !online && exists && previous.connID != 0 &&
				previous.connID != notification.ConnID
			p.logger.Debug("host presence notification", "online", online, "seq", notification.Sequence,
				"last_seq", p.lastSeq, "conn", notification.ConnID, "previous_conn", previous.connID,
				"superseded", superseded, "stale", exists && at.Before(previous.at))
			if !superseded && (!exists || !at.Before(previous.at)) {
				p.hosts[notification.HostID] = presenceEntry{online: online, at: at, connID: notification.ConnID}
			}
		}
		p.available = true
	case "device":
		if notification.DeviceID != "" {
			p.mu.Unlock()
			p.applyDevice(notification.DeviceID, online, at, PresenceSourceBroker, notification.ConnID)
			return
		}
	}
	p.mu.Unlock()
	p.publishCounts()
}

// ApplySnapshot replaces presence with the broker's current connection set at
// the returned sequence boundary.
func (p *Presence) ApplySnapshot(clients []SnapshotClient, boundary uint64) {
	p.mu.Lock()
	hosts := make(map[string]presenceEntry, len(p.hosts))
	devices := make(map[string]presenceEntry, len(p.devices))
	now := p.now()
	for _, client := range clients {
		if client.State != "connected" {
			continue
		}
		at := now
		if client.Connected > 0 {
			at = time.UnixMilli(client.Connected).UTC()
		}
		switch client.Kind {
		case "host":
			if client.HostID != "" {
				hosts[client.HostID] = presenceEntry{online: true, at: at, connID: client.ConnID}
			}
		case "device":
			if client.DeviceID != "" {
				devices[client.DeviceID] = presenceEntry{online: true, at: at, connID: client.ConnID}
			}
		}
	}
	p.hosts = hosts
	p.devices = devices
	if boundary > p.lastSeq {
		p.lastSeq = boundary
	}
	p.available = true
	p.mu.Unlock()
	p.publishCounts()
}

// deviceWillHint is the only information Access uses from a last-will payload.
type deviceWillHint struct {
	Event    string `json:"event"`
	DeviceID string `json:"device_id"`
}

const deviceOfflineEventName = "device_offline"

// HandleDeviceWill consumes a broker-published device last will. Unknown or
// invalid payloads are dropped and counted; they never invent a device or host.
func (p *Presence) HandleDeviceWill(topic string, payload []byte) {
	deviceID, ok := ParseDeviceWillTopic(topic)
	if !ok {
		if p.metrics != nil {
			p.metrics.PresenceInvalid()
		}
		return
	}
	var hint deviceWillHint
	if err := json.Unmarshal(payload, &hint); err != nil || hint.Event != deviceOfflineEventName {
		if p.metrics != nil {
			p.metrics.PresenceInvalid()
		}
		return
	}
	if hint.DeviceID != "" && hint.DeviceID != deviceID {
		if p.metrics != nil {
			p.metrics.PresenceInvalid()
		}
		return
	}
	p.applyDevice(deviceID, false, p.now(), PresenceSourceWill, 0)
}

// ConnectedCounts returns the last broker-derived online counts.
func (p *Presence) ConnectedCounts() (hosts, devices int) {
	p.mu.RLock()
	defer p.mu.RUnlock()
	if !p.available {
		return 0, 0
	}
	for _, entry := range p.hosts {
		if entry.online {
			hosts++
		}
	}
	for _, entry := range p.devices {
		if entry.online {
			devices++
		}
	}
	return hosts, devices
}

func (p *Presence) publishCounts() {
	if p.metrics == nil {
		return
	}
	hosts, devices := p.ConnectedCounts()
	p.metrics.SetConnected(hosts, devices)
}
