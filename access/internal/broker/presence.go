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
	// PresenceSourceSnapshot marks an offline edge proven by a later broker
	// snapshot: a disconnect notification was lost or missed while Access was
	// restarting, so the snapshot is the only current state.
	PresenceSourceSnapshot = "snapshot"
	// PresenceSourceExpired marks an offline edge caused by the cached
	// observation outliving presence.cache_ttl without a refresh. It bounds a
	// false-online entry when both live notifications and snapshots stall.
	PresenceSourceExpired = "expiry"
)

// DeviceOfflineHandler runs once for each accepted online -> offline edge.
// observedAt is the source timestamp used for stale-event rejection.
type DeviceOfflineHandler func(deviceID, source string, observedAt time.Time)

type presenceEntry struct {
	online bool
	// at is the source timestamp used to reject stale events for one identity.
	at time.Time
	// seenAt is when Access last observed this entry from a broker event or a
	// snapshot. It drives cache_ttl expiry independently of the broker's own
	// connection timestamp: a long-lived connection stays fresh because each
	// accepted event or snapshot refreshes it, while a lost disconnect stops
	// being refreshed and eventually expires.
	seenAt time.Time
	connID uint64
	// ready records that a Host confirmed subscription readiness with HostReady
	// (or that a broker snapshot already observed an established connection).
	// A broker CONNECT alone is not enough: the Host may not have subscribed
	// yet, so publishing then would be acknowledged by the broker but never
	// delivered.
	ready bool
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

// IsHostOnline returns broker presence. A missing, unavailable or expired
// presence view reports offline. A Host whose broker connection exists but has
// not confirmed subscription readiness (HostReady) is not online yet: the
// broker would accept a publish that no subscriber can receive.
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
	return entry.online && entry.ready && !p.stale(entry, p.now())
}

// stale reports whether a cached observation has outlived cache_ttl. The
// presence view must never claim a connection it has not observed within that
// bound.
func (p *Presence) stale(entry presenceEntry, now time.Time) bool {
	return p.cacheTTL > 0 && !entry.seenAt.IsZero() && now.Sub(entry.seenAt) > p.cacheTTL
}

// SetHostOnline updates broker-derived host presence. A direct online update
// from a caller that already established readiness (HostReady or a snapshot)
// also marks the connection ready.
func (p *Presence) SetHostOnline(hostID string, online bool, at time.Time) {
	if hostID == "" {
		return
	}
	now := p.now()
	p.mu.Lock()
	previous, exists := p.hosts[hostID]
	if !exists || !at.Before(previous.at) {
		p.hosts[hostID] = presenceEntry{online: online, at: at, seenAt: now, connID: previous.connID, ready: online}
	}
	p.available = true
	p.mu.Unlock()
	p.publishCounts()
}

// SetDeviceOnline updates broker-derived device presence. An online -> offline
// transition invokes the registered device-offline handler exactly once.
func (p *Presence) SetDeviceOnline(deviceID string, online bool, at time.Time) {
	p.applyDevice(deviceID, online, at, PresenceSourceBroker, 0, true)
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
//
// allowOnlineOverride is false for a device last will. A will payload does not
// name the connection that ended, so it cannot prove that the connection the
// cache currently holds is gone; a delayed will from a superseded connection
// must never mark a newer connection offline. Lost disconnect notifications are
// repaired by the next snapshot or by cache_ttl expiry instead.
func (p *Presence) applyDevice(deviceID string, online bool, at time.Time, source string, connID uint64, allowOnlineOverride bool) {
	if deviceID == "" {
		return
	}
	now := p.now()
	p.mu.Lock()
	previous, exists := p.devices[deviceID]
	if exists && at.Before(previous.at) {
		p.mu.Unlock()
		return
	}
	// A device connection notification with a newer connection identity is a
	// takeover and must be adopted. Only an offline event is fenced to its own
	// connection: a delayed disconnect for a superseded connection cannot
	// affect the newer one.
	if !online && connID != 0 && previous.connID != 0 && previous.connID != connID {
		p.mu.Unlock()
		p.logger.Debug("presence event for a superseded connection ignored", "source", source)
		return
	}
	if !online && !allowOnlineOverride && exists && previous.online {
		p.mu.Unlock()
		// Not a duplicate edge: the notification is unverifiable against the
		// live connection, so it is dropped without changing presence counts.
		p.logger.Debug("device will could not override a live connection", "source", source)
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
	next := presenceEntry{online: online, at: at, seenAt: now, connID: previous.connID}
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

// Expire marks cached observations older than cache_ttl offline. Live broker
// notifications and each successful snapshot refresh online entries, so a
// healthy reconcile loop never expires a connected client. A stalled presence
// path does: a lost disconnect must not keep a Host or device online forever.
// It returns the number of expired online entries.
func (p *Presence) Expire(now time.Time) int {
	p.mu.Lock()
	expired := 0
	for hostID, entry := range p.hosts {
		if entry.online && p.stale(entry, now) {
			p.hosts[hostID] = presenceEntry{online: false, at: entry.at, seenAt: now, connID: entry.connID}
			expired++
		}
	}
	var devices []string
	for deviceID, entry := range p.devices {
		if entry.online && p.stale(entry, now) {
			p.devices[deviceID] = presenceEntry{online: false, at: entry.at, seenAt: now, connID: entry.connID}
			devices = append(devices, deviceID)
			expired++
		}
	}
	handler := p.deviceOfflineHandler
	p.mu.Unlock()
	p.publishCounts()
	for _, deviceID := range devices {
		if p.metrics != nil {
			p.metrics.PresenceEvent(PresenceSourceExpired, "offline")
		}
		if handler != nil {
			handler(deviceID, PresenceSourceExpired, now)
		}
	}
	return expired
}

// MarkHostReady treats a ready host result as broker presence. HostReady is
// published only after the Host subscribed to its request topic, so it is the
// readiness confirmation a bare broker CONNECT lacks.
func (p *Presence) MarkHostReady(hostID string, at time.Time) {
	if hostID == "" {
		return
	}
	now := p.now()
	p.mu.Lock()
	previous := p.hosts[hostID]
	p.hosts[hostID] = presenceEntry{online: true, at: at, seenAt: now, connID: previous.connID, ready: true}
	p.mu.Unlock()
	p.publishCounts()
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
				ready := false
				if online && exists && previous.connID == notification.ConnID {
					// A repeated connect for the same connection keeps the
					// HostReady confirmation.
					ready = previous.ready
				}
				p.hosts[notification.HostID] = presenceEntry{
					online: online, at: at, seenAt: p.now(),
					connID: notification.ConnID, ready: ready,
				}
			}
		}
	case "device":
		if notification.DeviceID != "" {
			connID := notification.ConnID
			p.mu.Unlock()
			p.applyDevice(notification.DeviceID, online, at, PresenceSourceBroker, connID, true)
			return
		}
	}
	p.mu.Unlock()
	p.publishCounts()
}

// ApplySnapshot replaces presence with the broker's current connection set at
// the returned sequence boundary. An online device that is absent or offline in
// the snapshot lost its offline edge (a missed or startup-window notification)
// and is emitted as offline here; this is the sequence-boundary self-heal.
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
				// A snapshot connection predates this Access session, so its
				// subscription was already established. Only a live CONNECT
				// after the snapshot waits for HostReady.
				hosts[client.HostID] = presenceEntry{online: true, at: at, seenAt: now, connID: client.ConnID, ready: true}
			}
		case "device":
			if client.DeviceID != "" {
				devices[client.DeviceID] = presenceEntry{online: true, at: at, seenAt: now, connID: client.ConnID}
			}
		}
	}
	var recovered []string
	for deviceID, previous := range p.devices {
		if !previous.online {
			continue
		}
		next, ok := devices[deviceID]
		if ok && next.online {
			continue
		}
		recovered = append(recovered, deviceID)
	}
	p.hosts = hosts
	p.devices = devices
	if boundary > p.lastSeq {
		p.lastSeq = boundary
	}
	p.available = true
	handler := p.deviceOfflineHandler
	p.mu.Unlock()
	p.publishCounts()
	for _, deviceID := range recovered {
		if p.metrics != nil {
			p.metrics.PresenceEvent(PresenceSourceSnapshot, "offline")
		}
		if handler != nil {
			handler(deviceID, PresenceSourceSnapshot, now)
		}
	}
}

// Sweep expires cached observations using the presence clock. The line
// heartbeat calls it so a stalled reconcile cannot leave a stale online entry
// until the next successful snapshot.
func (p *Presence) Sweep() int { return p.Expire(p.now()) }

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
	p.applyDevice(deviceID, false, p.now(), PresenceSourceWill, 0, false)
}

// ConnectedCounts returns the last broker-derived online counts, excluding
// observations that outlived cache_ttl.
func (p *Presence) ConnectedCounts() (hosts, devices int) {
	p.mu.RLock()
	defer p.mu.RUnlock()
	if !p.available {
		return 0, 0
	}
	now := p.now()
	for _, entry := range p.hosts {
		if entry.online && entry.ready && !p.stale(entry, now) {
			hosts++
		}
	}
	for _, entry := range p.devices {
		if entry.online && !p.stale(entry, now) {
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
