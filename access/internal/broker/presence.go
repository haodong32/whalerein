package broker

import (
	"context"

	"log/slog"
	"sort"
	"sync"
	"time"

	"github.com/whalerein/whalerein/access/internal/diagnostics"
)

// Presence sources for device lifecycle events. Native broker notifications
// and reconciliation snapshots supply device lifecycle evidence.
const (
	PresenceSourceBroker = "broker"
	// PresenceSourceSnapshot marks an offline edge proven by a later broker
	// snapshot: a disconnect notification was lost or missed while Access was
	// restarting, so the snapshot is the only current state.
	PresenceSourceSnapshot = "snapshot"
	// PresenceSourceExpired marks an offline edge caused by the cached
	// observation outliving presence.cache_ttl without a refresh. It bounds a
	// false-online entry when both live notifications and snapshots stall.
	PresenceSourceExpired = "expiry"
)

// ConnectionClosedHandler releases work owned by one former connection.
// observedAt is the source timestamp used for stale-event rejection.
type ConnectionClosedHandler func(deviceID, source string, observedAt time.Time, ref ConnectionRef)

// liveConnection is one admitted device connection the presence registry can
// target for a scoped revocation. The client id is carried because a device
// may use the fixed id or a bounded suffixed variant.
type liveConnection struct {
	sessionID string
	clientID  string
	seq       uint64
	closed    bool
}

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
	// epoch is the broker-process epoch of connID. A changed epoch invalidates
	// the connection identity for scoped revocation.
	epoch string
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
// broker connection notifications, a broker snapshot or
// a Host Ready signal. It never queries a database heartbeat.
type Presence struct {
	mu          sync.RWMutex
	hosts       map[string]presenceEntry
	devices     map[string]presenceEntry
	connections map[string]map[ConnectionRef]liveConnection
	// epoch is the broker-process epoch the current snapshot belongs to. A
	// changed epoch invalidates every tracked connection reference.
	epoch                   string
	cacheTTL                time.Duration
	available               bool
	lastSeq                 uint64
	metrics                 *diagnostics.Metrics
	logger                  *slog.Logger
	now                     func() time.Time
	onEpochChange           func()
	connectionClosedHandler ConnectionClosedHandler
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
		connections: make(map[string]map[ConnectionRef]liveConnection),
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

// SetDeviceOnline updates aggregate directory presence without naming a connection.
func (p *Presence) SetDeviceOnline(deviceID string, online bool, at time.Time) {
	p.mu.Lock()
	p.applyDeviceLocked(deviceID, online, at, 0, 0)
	p.mu.Unlock()
	p.publishCounts()
}

// SetConnectionClosedHandler installs the callback for exact connection removals.
// The callback runs outside the presence lock.
func (p *Presence) SetConnectionClosedHandler(handler ConnectionClosedHandler) {
	p.mu.Lock()
	p.connectionClosedHandler = handler
	p.mu.Unlock()
}

// applyDeviceLocked updates aggregate presence under the same lock as the connection registry.
func (p *Presence) applyDeviceLocked(deviceID string, online bool, at time.Time, connID uint64, seq uint64) {
	if deviceID == "" {
		return
	}
	now := p.now()
	previous, exists := p.devices[deviceID]
	if exists && (at.Before(previous.at) || (seq != 0 && previous.seq > seq)) {

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

		if p.metrics != nil {
			p.metrics.PresenceEvent(PresenceSourceBroker, "online")
		}
		return
	}
	if connID != 0 {
		if _, tracked := connections[connID]; !tracked {
			// A delayed disconnect for a superseded connection cannot remove
			// the newer live connection or mark it offline.
			if exists && previous.connID != 0 && previous.connID != connID {

				p.logger.Debug("presence event for a superseded connection ignored", "source", PresenceSourceBroker)
				return
			}
			// The entry has no tracked connection for this identity; treat it
			// as the disconnect of the last known connection.
			clear(connections)
		} else {
			delete(connections, connID)
		}
	} else {
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

		return
	}
	duplicate := exists && !previous.online
	p.devices[deviceID] = presenceEntry{online: false, at: at, seenAt: now, connID: previous.connID, seq: seq}

	if duplicate {
		if p.metrics != nil {
			p.metrics.PresenceDuplicate()
		}
		return
	}
	if p.metrics != nil {
		p.metrics.PresenceEvent(PresenceSourceBroker, "offline")
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
	var closed []closedConnection
	for deviceID, entry := range p.devices {
		if entry.online && p.stale(entry, now) {
			p.devices[deviceID] = presenceEntry{online: false, at: entry.at, seenAt: now, connID: entry.connID, seq: entry.seq}
			for ref, conn := range p.connections[deviceID] {
				if _, belongs := entry.connections[ref.ID]; belongs && !conn.closed {
					closed = append(closed, closedConnection{deviceID, ref})
					delete(p.connections[deviceID], ref)
				}
			}
			if p.metrics != nil {
				p.metrics.PresenceEvent(PresenceSourceExpired, "offline")
			}
			expired++
		}
	}
	handler := p.connectionClosedHandler
	p.mu.Unlock()
	p.publishCounts()
	if handler != nil {
		for _, c := range closed {
			handler(c.device, PresenceSourceExpired, now, c.ref)
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
	_, active := p.DeviceConnectionSession(deviceID, ref)
	return active
}

// DeviceConnectionSession returns the authenticated managed session, or the
// empty string for an active standalone Device connection.
func (p *Presence) DeviceConnectionSession(deviceID string, ref ConnectionRef) (string, bool) {
	if deviceID == "" || !ref.Valid() {
		return "", false
	}
	p.mu.RLock()
	defer p.mu.RUnlock()
	conn, ok := p.connections[deviceID][ref]
	return conn.sessionID, ok && !conn.closed
}

// DeviceConnectionRefs returns the current broker connection references for a
// device, for diagnostics and tests.
func (p *Presence) DeviceConnectionRefs(deviceID string) []ConnectionRef {
	if deviceID == "" {
		return nil
	}
	connections := p.DeviceConnections(deviceID)
	refs := make([]ConnectionRef, 0, len(connections))
	for _, connection := range connections {
		refs = append(refs, connection.Ref)
	}
	return refs
}

// DeviceConnections returns the admitted broker connections of a device with
// their client ids, ordered deterministically so a retry issues the same
// scoped control operations.
func (p *Presence) DeviceConnections(deviceID string) []ScopeConnection {
	if deviceID == "" {
		return nil
	}
	p.mu.RLock()
	live := p.connections[deviceID]
	out := make([]ScopeConnection, 0, len(live))
	for ref, connection := range live {
		if connection.closed || !ref.Valid() {
			continue
		}
		out = append(out, ScopeConnection{Ref: ref, ClientID: connection.clientID})
	}
	p.mu.RUnlock()
	sort.Slice(out, func(i, j int) bool {
		if out[i].ClientID != out[j].ClientID {
			return out[i].ClientID < out[j].ClientID
		}
		return out[i].Ref.ID < out[j].Ref.ID
	})
	return out
}

// HostConnectionRef returns the admitted broker connection identity of one
// Host. A Host re-add establishes a different enrollment generation and a new
// connection instance, so a delayed revocation targets only the frozen ref.
func (p *Presence) HostConnectionRef(hostID string) (ConnectionRef, bool) {
	connection, ok := p.HostScopeConnection(hostID)
	return connection.Ref, ok
}

// HostScopeConnection returns the admitted Host connection together with its
// deterministic broker client id.
func (p *Presence) HostScopeConnection(hostID string) (ScopeConnection, bool) {
	if hostID == "" {
		return ScopeConnection{}, false
	}
	p.mu.RLock()
	defer p.mu.RUnlock()
	entry, ok := p.hosts[hostID]
	if !ok || !entry.online || entry.connID == 0 {
		return ScopeConnection{}, false
	}
	epoch := entry.epoch
	if epoch == "" {
		epoch = p.epoch
	}
	ref := ConnectionRef{Epoch: epoch, ID: entry.connID}
	if !ref.Valid() {
		return ScopeConnection{}, false
	}
	connection := ScopeConnection{Ref: ref, ClientID: ClientIDForHost(hostID)}
	return connection, connection.Valid()
}

// BrokerEpoch returns the broker-process epoch of the current snapshot.
func (p *Presence) BrokerEpoch() string {
	p.mu.RLock()
	defer p.mu.RUnlock()
	return p.epoch
}

type closedConnection struct {
	device string
	ref    ConnectionRef
}

func (p *Presence) noteConnectionLocked(deviceID string, ref ConnectionRef, clientID, sessionID string, online bool, sequence uint64) bool {
	if !ref.Valid() {
		return false
	}
	if p.connections[deviceID] == nil {
		p.connections[deviceID] = make(map[ConnectionRef]liveConnection)
	}
	old, tracked := p.connections[deviceID][ref]
	// Keep a disconnect until a snapshot includes its sequence. Otherwise a
	// snapshot already in flight could put a closed connection back into use.
	p.connections[deviceID][ref] = liveConnection{clientID: clientID, sessionID: sessionID, seq: sequence, closed: !online}
	return !online && tracked && !old.closed
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
	if p.epoch != "" && notification.Epoch != "" && notification.Epoch != p.epoch {
		p.mu.Unlock()
		return
	}
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
				epoch := notification.Epoch
				if epoch == "" {
					epoch = p.epoch
				}
				p.hosts[notification.HostID] = presenceEntry{
					online: online, at: at, seenAt: p.now(),
					connID: notification.ConnID, epoch: epoch, ready: ready, seq: notification.Sequence,
				}
			}
		}
	case "device", "session":
		device := notification.DeviceID
		if device != "" {
			if previous, ok := p.devices[device]; notification.Kind == "device" && ok && at.Before(previous.at) {
				break
			}
			if online && notification.Kind == "session" && notification.SessionID == "" {
				break
			}
			epoch := notification.Epoch
			if epoch == "" {
				epoch = p.epoch
			}
			ref := ConnectionRef{Epoch: epoch, ID: notification.ConnID}
			closed := p.noteConnectionLocked(device, ref, notification.ClientID, notification.SessionID, online, notification.Sequence)
			handler := p.connectionClosedHandler
			if notification.Kind == "device" {
				p.applyDeviceLocked(device, online, at, ref.ID, notification.Sequence)
			}
			p.mu.Unlock()
			p.publishCounts()
			if closed && handler != nil {
				handler(device, PresenceSourceBroker, at, ref)
			}
			return
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
	if p.epoch != "" {
		// Sequence numbers and connection ids start again at broker boot.
		// The new snapshot must never merge entries from an earlier process.
		if p.onEpochChange != nil {
			p.onEpochChange()
		}
		clear(p.hosts)
		clear(p.devices)
		clear(p.connections)
		p.lastSeq = 0
		p.available = false
	}
	p.epoch = epoch
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
				epoch := client.Epoch
				if epoch == "" {
					epoch = p.epoch
				}
				hosts[client.HostID] = presenceEntry{online: true, at: at, seenAt: now, connID: client.ConnID, epoch: epoch, ready: ready, seq: boundary}
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
	for deviceID, previous := range p.devices {
		if previous.seq > boundary {
			previous.seenAt = now
			devices[deviceID] = previous
		} else if previous.online && !devices[deviceID].online && p.metrics != nil {
			p.metrics.PresenceEvent(PresenceSourceSnapshot, "offline")
		}
	}
	connections := make(map[string]map[ConnectionRef]liveConnection)
	for _, client := range clients {
		if (client.Kind != "device" && client.Kind != "session") || (client.Kind == "session" && client.SessionID == "") || client.State != "connected" || client.DeviceID == "" {
			continue
		}
		epoch := client.Epoch
		if epoch == "" {
			epoch = p.epoch
		}
		ref := ConnectionRef{Epoch: epoch, ID: client.ConnID}
		if !ref.Valid() {
			continue
		}
		if connections[client.DeviceID] == nil {
			connections[client.DeviceID] = make(map[ConnectionRef]liveConnection)
		}
		connections[client.DeviceID][ref] = liveConnection{clientID: client.ClientID, sessionID: client.SessionID, seq: boundary}
	}
	var closed []closedConnection
	for device, refs := range p.connections {
		for ref, conn := range refs {
			if conn.seq > boundary {
				if connections[device] == nil {
					connections[device] = make(map[ConnectionRef]liveConnection)
				}
				connections[device][ref] = conn
			} else if _, present := connections[device][ref]; !present && !conn.closed {
				closed = append(closed, closedConnection{device, ref})
			}
		}
	}
	p.hosts = hosts
	p.devices = devices
	p.connections = connections
	if boundary > p.lastSeq {
		p.lastSeq = boundary
	}
	p.available = true
	handler := p.connectionClosedHandler
	p.mu.Unlock()
	p.publishCounts()
	if handler != nil {
		for _, c := range closed {
			handler(c.device, PresenceSourceSnapshot, now, c.ref)
		}
	}
}

// Sweep expires cached observations using the presence clock. The line
// heartbeat calls it so a stalled reconcile cannot leave a stale online entry
// until the next successful snapshot.
func (p *Presence) Sweep() int { return p.Expire(p.now()) }

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
