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
	// connections is the set of live broker connection identities for one
	// device. A device can hold several authenticated connections at once (for
	// example a long-lived watch connection and a short-lived send connection);
	// presence is offline only when this set becomes empty. Hosts leave it nil.
	connections map[uint64]struct{}
	// ready records that a Host confirmed subscription readiness with HostReady
	// (or that a broker snapshot already observed an established connection).
	// A broker CONNECT alone is not enough: the Host may not have subscribed
	// yet, so publishing then would be acknowledged by the broker but never
	// delivered.
	ready bool
	// seq is the broker notification sequence that established this entry. A
	// snapshot reconciles at a boundary; an entry observed after that boundary
	// describes newer broker state and must not be rolled back by a snapshot
	// taken before it.
	seq uint64
}

// Presence is the broker-derived online registry. It is seeded by ordered
// broker connection notifications, a broker snapshot, the device last will or
// a Host Ready signal. It never queries a database heartbeat.
type Presence struct {
	mu          sync.RWMutex
	hosts       map[string]presenceEntry
	devices     map[string]presenceEntry
	connections map[string]map[ConnectionRef]struct{}
	// epoch is the broker-process epoch the current snapshot belongs to. A
	// changed epoch invalidates every tracked connection reference.
	epoch                string
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
		hosts:       make(map[string]presenceEntry),
		devices:     make(map[string]presenceEntry),
		connections: make(map[string]map[ConnectionRef]struct{}),
		cacheTTL:    cacheTTL,
		metrics:     metrics,
		logger:      logger,
		now:         func() time.Time { return time.Now().UTC() },
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
func (p *Presence) IsHostOnline(_ context.Context, hostID string) bool {
	online, _ := p.HostPresence(hostID)
	return online
}

// HostPresence distinguishes offline from an unreconciled broker view.
func (p *Presence) HostPresence(hostID string) (online, known bool) {
	if hostID == "" {
		return false, false
	}
	p.mu.RLock()
	defer p.mu.RUnlock()
	if !p.available {
		return false, false
	}
	entry, ok := p.hosts[hostID]
	if !ok {
		return false, true
	}
	return entry.online && entry.ready && !p.stale(entry, p.now()), true
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
	p.applyDevice(deviceID, online, at, PresenceSourceBroker, 0, true, 0)
}

// SetDeviceOfflineHandler installs the callback for accepted device-offline
// transitions. The callback runs outside the presence lock.
func (p *Presence) SetDeviceOfflineHandler(handler DeviceOfflineHandler) {
	p.mu.Lock()
	p.deviceOfflineHandler = handler
	p.mu.Unlock()
}

// applyDevice records device presence and emits at most one offline callback
// per online -> offline edge. A non-zero connID is added to or removed from the
// device's live connection set; the device is offline only when that set becomes
// empty. A delayed disconnect for a superseded connection therefore cannot
// affect a newer or sibling live connection.
//
// allowOnlineOverride is false for a device last will. A will payload does not
// name the connection that ended, so it cannot prove that the connection the
// cache currently holds is gone; a delayed will from a superseded connection
// must never mark a newer connection offline. Lost disconnect notifications are
// repaired by the next snapshot or by cache_ttl expiry instead.
func (p *Presence) applyDevice(deviceID string, online bool, at time.Time, source string, connID uint64, allowOnlineOverride bool, seq uint64) {
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
	connections := cloneConnections(previous.connections)
	if online {
		nextConnID := previous.connID
		if connID != 0 {
			connections[connID] = struct{}{}
			if connID > nextConnID {
				nextConnID = connID
			}
		}
		p.devices[deviceID] = presenceEntry{
			online: true, at: at, seenAt: now, connID: nextConnID,
			connections: connections, seq: seq,
		}
		p.mu.Unlock()
		p.publishCounts()
		if p.metrics != nil {
			p.metrics.PresenceEvent(source, "online")
		}
		return
	}
	if connID != 0 {
		if _, tracked := connections[connID]; !tracked {
			// A delayed disconnect for a superseded connection cannot remove
			// the newer live connection or mark it offline.
			if exists && previous.connID != 0 && previous.connID != connID {
				p.mu.Unlock()
				p.logger.Debug("presence event for a superseded connection ignored", "source", source)
				return
			}
			// The entry has no tracked connection for this identity; treat it
			// as the disconnect of the last known connection.
			clear(connections)
		} else {
			delete(connections, connID)
		}
	} else {
		// A last will names no connection. It can only prove an offline edge
		// when no tracked connection remains; a sibling connection may still be
		// live.
		if !allowOnlineOverride && len(connections) > 0 {
			p.mu.Unlock()
			p.logger.Debug("device will could not override a live connection", "source", source)
			return
		}
		clear(connections)
	}
	if len(connections) > 0 {
		nextConnID := uint64(0)
		for liveConn := range connections {
			if liveConn > nextConnID {
				nextConnID = liveConn
			}
		}
		p.devices[deviceID] = presenceEntry{
			online: true, at: at, seenAt: now, connID: nextConnID,
			connections: connections, seq: seq,
		}
		p.mu.Unlock()
		p.publishCounts()
		p.logger.Debug("presence event for a sibling connection ignored", "source", source)
		return
	}
	transition := !exists || previous.online
	duplicate := exists && !previous.online
	p.devices[deviceID] = presenceEntry{online: false, at: at, seenAt: now, connID: previous.connID, seq: seq}
	handler := p.deviceOfflineHandler
	p.mu.Unlock()

	p.publishCounts()
	if duplicate {
		if p.metrics != nil {
			p.metrics.PresenceDuplicate()
		}
		return
	}
	if p.metrics != nil {
		p.metrics.PresenceEvent(source, "offline")
	}
	if transition && handler != nil {
		handler(deviceID, source, at)
	}
}
func cloneConnections(connections map[uint64]struct{}) map[uint64]struct{} {
	if len(connections) == 0 {
		return map[uint64]struct{}{}
	}
	clone := make(map[uint64]struct{}, len(connections))
	for connID := range connections {
		clone[connID] = struct{}{}
	}
	return clone
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
			p.hosts[hostID] = presenceEntry{online: false, at: entry.at, seenAt: now, connID: entry.connID, ready: entry.ready, seq: entry.seq}
			expired++
		}
	}
	var devices []string
	for deviceID, entry := range p.devices {
		if entry.online && p.stale(entry, now) {
			p.devices[deviceID] = presenceEntry{online: false, at: entry.at, seenAt: now, connID: entry.connID, seq: entry.seq}
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

// readyHandshakeTolerance bounds the unavoidable ordering gap between the
// broker's CONNECT observation and the HostReady publication.
//
// Mosquitto hands the client its CONNACK before the login plugin's connect
// notification reaches Access, and the Host subscribes and publishes HostReady
// as soon as the connection is usable. The proof's timestamp is therefore a
// few milliseconds *earlier* than the connect timestamp Access recorded, even
// though it belongs to the current connection. A strict timestamp comparison
// would reject every real readiness proof and report a connected Host offline.
//
// A proof older than this window cannot belong to the current connection: a
// superseded connection's proof is delayed by at least a full reconnect, and
// the accepted window stays far below a reconnect cycle. The window is not a
// clock-skew allowance; both timestamps come from the same host.
const readyHandshakeTolerance = 2 * time.Second

// MarkHostReady treats a ready host result as broker presence. HostReady is
// published only after the Host subscribed to its request topic, so it is the
// readiness confirmation a bare broker CONNECT lacks. at is the HostReady
// publication timestamp. A delayed readiness proof from a superseded
// connection predates the current connection observation and cannot confirm
// the replacement; the replacement needs its own HostReady. It reports whether
// this proof was accepted for the current connection.
func (p *Presence) MarkHostReady(hostID string, at time.Time) bool {
	if hostID == "" {
		return false
	}
	now := p.now()
	p.mu.Lock()
	previous, exists := p.hosts[hostID]
	if exists && at.Before(previous.at.Add(-readyHandshakeTolerance)) {
		p.logger.Debug("stale host readiness proof ignored", "conn", previous.connID)
		p.mu.Unlock()
		return false
	}
	if exists {
		p.hosts[hostID] = presenceEntry{online: true, at: previous.at, seenAt: now,
			connID: previous.connID, ready: true, seq: previous.seq}
	} else {
		p.hosts[hostID] = presenceEntry{online: true, at: at, seenAt: now, ready: true}
	}
	p.mu.Unlock()
	p.publishCounts()
	return true
}

// HostConnection returns the broker-derived connection identity and observation
// instant for one host. Receivers bind their advertisement to this identity so a
// reconnect invalidates the prior entry.
func (p *Presence) HostConnection(hostID string) (connID uint64, observedAt time.Time, ready bool, known bool) {
	if hostID == "" {
		return 0, time.Time{}, false, false
	}
	p.mu.RLock()
	defer p.mu.RUnlock()
	entry, ok := p.hosts[hostID]
	if !ok {
		return 0, time.Time{}, false, false
	}
	return entry.connID, entry.at, entry.ready, true
}

// DeviceConnection returns the broker-derived connection identity for one
// device, used to invalidate device receive advertisements on reconnect.
func (p *Presence) DeviceConnection(deviceID string) (connID uint64, known bool) {
	if deviceID == "" {
		return 0, false
	}
	p.mu.RLock()
	defer p.mu.RUnlock()
	entry, ok := p.devices[deviceID]
	if !ok {
		return 0, false
	}
	return entry.connID, true
}

// DeviceConnectionActive reports whether ref is one of the current broker
// connections for the device. A replacement or broker-epoch change invalidates
// every older reference, so a stale query stamp is rejected.
func (p *Presence) DeviceConnectionActive(deviceID string, ref ConnectionRef) bool {
	if deviceID == "" || !ref.Valid() {
		return false
	}
	p.mu.RLock()
	defer p.mu.RUnlock()
	_, ok := p.connections[deviceID][ref]
	return ok
}

// DeviceConnectionRefs returns the current broker connection references for a
// device, for diagnostics and tests.
func (p *Presence) DeviceConnectionRefs(deviceID string) []ConnectionRef {
	if deviceID == "" {
		return nil
	}
	p.mu.RLock()
	defer p.mu.RUnlock()
	refs := make([]ConnectionRef, 0, len(p.connections[deviceID]))
	for ref := range p.connections[deviceID] {
		refs = append(refs, ref)
	}
	return refs
}

func (p *Presence) noteConnectionLocked(deviceID string, ref ConnectionRef, online bool) {
	if deviceID == "" || !ref.Valid() {
		return
	}
	if online {
		if p.connections[deviceID] == nil {
			p.connections[deviceID] = map[ConnectionRef]struct{}{}
		}
		p.connections[deviceID][ref] = struct{}{}
		return
	}
	delete(p.connections[deviceID], ref)
	if len(p.connections[deviceID]) == 0 {
		delete(p.connections, deviceID)
	}
}

// HandleNotification applies one ordered broker connection notification.
func (p *Presence) HandleNotification(notification *Notification) {
	if notification == nil {
		return
	}
	p.mu.Lock()
	epoch := p.epoch
	p.mu.Unlock()
	if epoch != "" && notification.Epoch != "" && notification.Epoch != epoch {
		return
	}
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
					connID: notification.ConnID, ready: ready, seq: notification.Sequence,
				}
			}
		}
	case "device":
		if notification.DeviceID != "" {
			connID := notification.ConnID
			p.noteConnectionLocked(notification.DeviceID, ConnectionRef{Epoch: notification.Epoch, ID: notification.ConnID}, online)
			p.mu.Unlock()
			p.applyDevice(notification.DeviceID, online, at, PresenceSourceBroker, connID, true, notification.Sequence)
			return
		}
	case "session":
		// Managed sessions are device connections for query admission and
		// reply affinity, but their lifecycle does not create directory
		// presence edges.
		if notification.DeviceID != "" {
			p.noteConnectionLocked(notification.DeviceID, ConnectionRef{Epoch: notification.Epoch, ID: notification.ConnID}, online)
		}
	}
	p.mu.Unlock()
	p.publishCounts()
}

// SetBrokerEpoch records the broker-process epoch of the current snapshot.
// A change invalidates every tracked connection reference, so delayed events
// from the previous process cannot affect the rebuilt presence. It returns
// true when the epoch changed.
func (p *Presence) SetBrokerEpoch(epoch string) bool {
	if epoch == "" {
		return false
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.epoch == epoch {
		return false
	}
	p.epoch = epoch
	p.connections = make(map[string]map[ConnectionRef]struct{}, len(p.devices))
	return true
}

// ApplySnapshot replaces presence with the broker's current connection set at
// the returned sequence boundary. An online device that is absent or offline in
// the snapshot lost its offline edge (a missed or startup-window notification)
// and is emitted as offline here; this is the sequence-boundary self-heal.
//
// A live CONNECT notification can be applied before its reconciliation reply.
// Such an entry carries a sequence after the snapshot boundary and describes
// newer broker state than the snapshot, so it is retained instead of being
// rolled back to the older connection. A replacement connection first seen in
// a snapshot stays unready until its own HostReady: only the initial snapshot
// of an Access session recovers readiness for connections that were already
// established.
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
				previous, exists := p.hosts[client.HostID]
				if exists && previous.seq > boundary {
					previous.seenAt = now
					hosts[client.HostID] = previous
					continue
				}
				// Initial snapshots recover connections established before this
				// Access session started. Any later replacement connection
				// observed first in a snapshot must still prove subscription
				// readiness with its own HostReady.
				ready := true
				if exists {
					ready = previous.ready && (previous.connID == client.ConnID || previous.connID == 0)
				}
				hosts[client.HostID] = presenceEntry{online: true, at: at, seenAt: now, connID: client.ConnID, ready: ready, seq: boundary}
			}
		case "device":
			if client.DeviceID != "" {
				previous, exists := p.devices[client.DeviceID]
				if exists && previous.seq > boundary {
					previous.seenAt = now
					devices[client.DeviceID] = previous
					continue
				}
				entry, seen := devices[client.DeviceID]
				if !seen {
					entry = presenceEntry{
						at: at, seenAt: now,
						connections: map[uint64]struct{}{}, seq: boundary,
					}
				}
				if client.ConnID != 0 {
					entry.connections[client.ConnID] = struct{}{}
					if client.ConnID > entry.connID {
						entry.connID = client.ConnID
					}
				}
				if at.After(entry.at) {
					entry.at = at
				}
				entry.online = len(entry.connections) > 0
				devices[client.DeviceID] = entry
			}
		}
	}
	// A newer live event may not have reached the snapshot yet. Keep it instead
	// of treating the older snapshot as authoritative.
	for hostID, previous := range p.hosts {
		if previous.seq <= boundary {
			continue
		}
		if _, ok := hosts[hostID]; ok {
			continue
		}
		previous.seenAt = now
		hosts[hostID] = previous
	}
	var recovered []string
	for deviceID, previous := range p.devices {
		if !previous.online {
			continue
		}
		if previous.seq > boundary {
			previous.seenAt = now
			devices[deviceID] = previous
			continue
		}
		next, ok := devices[deviceID]
		if ok && next.online {
			continue
		}
		recovered = append(recovered, deviceID)
	}
	// The snapshot is the authoritative current connection set; every stale
	// reference from an older broker epoch or replaced connection disappears.
	connections := make(map[string]map[ConnectionRef]struct{}, len(devices))
	for _, client := range clients {
		if client.Kind != "device" || client.State != "connected" || client.DeviceID == "" {
			continue
		}
		ref := ConnectionRef{Epoch: client.Epoch, ID: client.ConnID}
		if !ref.Valid() {
			continue
		}
		if connections[client.DeviceID] == nil {
			connections[client.DeviceID] = map[ConnectionRef]struct{}{}
		}
		connections[client.DeviceID][ref] = struct{}{}
	}
	// A CONNECT notification applied after the snapshot boundary describes
	// newer broker state; keep its reference instead of rolling it back.
	for deviceID := range p.devices {
		if p.devices[deviceID].seq <= boundary {
			continue
		}
		for ref := range p.connections[deviceID] {
			if connections[deviceID] == nil {
				connections[deviceID] = map[ConnectionRef]struct{}{}
			}
			connections[deviceID][ref] = struct{}{}
		}
	}
	p.hosts = hosts
	p.devices = devices
	p.connections = connections
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
	p.applyDevice(deviceID, false, p.now(), PresenceSourceWill, 0, false, 0)
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
