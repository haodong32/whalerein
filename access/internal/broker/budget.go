package broker

import (
	"errors"
	"fmt"
	"math"
	"sync"
	"time"

	"github.com/whalerein/whalerein/access/internal/config"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// Stable transport rejection codes. They are the only labels emitted to logs
// and metrics; they never carry a topic, peer identity or payload.
const (
	transportCodePacket     = "packet_limit"
	transportCodeHeader     = "envelope_header"
	transportCodeEnvelope   = "envelope_budget"
	transportCodeReference  = "reference_limit"
	transportCodeTopic      = "relay_topic"
	transportCodeStaging    = "staging"
	transportCodeQueue      = "queue"
	transportCodeBudget     = "budget_missing"
	transportCodeBudgetBad  = "budget_invalid"
	transportCodeRelayEpoch = "relay_epoch"
)

var transportCodes = map[string]struct{}{
	transportCodePacket: {}, transportCodeHeader: {}, transportCodeEnvelope: {},
	transportCodeReference: {}, transportCodeTopic: {}, transportCodeStaging: {},
	transportCodeQueue: {}, transportCodeBudget: {}, transportCodeBudgetBad: {},
	transportCodeRelayEpoch: {},
}

// RelayEpoch is the only accepted complete-message relay contract.
const RelayEpoch uint32 = 3

// Typed relay errors. Callers map these to stable wire outcomes; they never
// include a topic, peer identity or payload byte.
var (
	// ErrPeerTransportUnsupported means the peer does not advertise the relay
	// epoch or cannot take a complete message.
	ErrPeerTransportUnsupported = errors.New("peer does not support the relay epoch")
	// ErrMessageBudget is a permanent complete-message refusal (payload_too_large).
	ErrMessageBudget = errors.New("complete message exceeds the relay budget")
	// ErrRelayEpoch is a missing or foreign relay epoch (protocol_version_incompatible).
	ErrRelayEpoch = fmt.Errorf("%w: relay epoch is not v3", ErrPeerTransportUnsupported)
	// ErrReferenceTooLarge means the reference packet itself cannot fit.
	ErrReferenceTooLarge = fmt.Errorf("%w: reference packet exceeds the packet limit", ErrMessageBudget)
	// ErrStagingUnavailable means a reference was required but no stager is wired.
	ErrStagingUnavailable = fmt.Errorf("%w: payload staging is unavailable", ErrPeerTransportUnsupported)
	// ErrStagingFailed is a bounded staging failure; nothing was published.
	ErrStagingFailed = fmt.Errorf("%w: payload staging failed", ErrPeerTransportUnsupported)
	// ErrQueueFull and ErrQueueBytes are transient ordered-queue refusals.
	ErrQueueFull  = errors.New("ordered publication queue is full")
	ErrQueueBytes = fmt.Errorf("%w: waiting bytes exceed the queue bound", ErrQueueFull)
	// ErrQueueClosed means the MQTT connection that owned the queue is gone.
	ErrQueueClosed = errors.New("ordered publication queue is closed")
)

// TransportError is a rejected packet or admission decision with a stable code.
// It never includes peer identifiers or payload content.
type TransportError struct {
	Code string
	err  error
}

func (e *TransportError) Error() string {
	if e.err == nil {
		return "relay rejected"
	}
	return e.err.Error()
}

func (e *TransportError) Unwrap() error { return e.err }

func transportReject(code string, err error) error {
	if _, ok := transportCodes[code]; !ok {
		code = transportCodeHeader
	}
	return &TransportError{Code: code, err: err}
}

// transportCodeOf returns the bounded code for a rejection.
func transportCodeOf(err error) string {
	var rejected *TransportError
	if errors.As(err, &rejected) {
		return rejected.Code
	}
	return transportCodeHeader
}

// validateRelayEpoch checks the single required relay epoch.
func validateRelayEpoch(version uint32) error {
	if version != RelayEpoch {
		return transportReject(transportCodeRelayEpoch, ErrRelayEpoch)
	}
	return nil
}

// validateAdvertisement checks one peer receive advertisement for internal
// consistency. Its named profile selects the packaged base vocabulary and
// supplies the packet framing allowance, but the advertised values are not
// required to equal it: an operator may tighten or widen individual limits, and
// directions may differ. Only the relations that make the budget safe are
// enforced. The sender intersects this budget with its own local budget, so a
// peer still cannot raise a local limit.
func validateAdvertisement(advertisement *whalereinv1.ReceiveAdvertisement) (config.TransportEffective, uint64, error) {
	if advertisement == nil {
		return config.TransportEffective{}, 0, transportReject(transportCodeBudget,
			fmt.Errorf("%w: receive advertisement missing", ErrPeerTransportUnsupported))
	}
	if err := validateRelayEpoch(advertisement.GetRelayVersion()); err != nil {
		return config.TransportEffective{}, 0, err
	}
	if advertisement.GetConnectionGeneration() == 0 {
		return config.TransportEffective{}, 0, transportReject(transportCodeBudgetBad,
			fmt.Errorf("%w: receive advertisement generation missing", ErrPeerTransportUnsupported))
	}
	profile := advertisement.GetProfile()
	wireName, known := whalereinv1.TransportProfile_name[int32(profile)]
	if !known || profile == whalereinv1.TransportProfile_TRANSPORT_PROFILE_UNSPECIFIED {
		return config.TransportEffective{}, 0, transportReject(transportCodeBudgetBad,
			fmt.Errorf("%w: unknown transport profile", ErrPeerTransportUnsupported))
	}
	profileName, ok := config.ProfileForWireName(wireName)
	if !ok {
		return config.TransportEffective{}, 0, transportReject(transportCodeBudgetBad,
			fmt.Errorf("%w: unknown transport profile", ErrPeerTransportUnsupported))
	}
	base, ok := whalereinv1.TransportProfileValuesFor(profileName)
	if !ok {
		return config.TransportEffective{}, 0, transportReject(transportCodeBudgetBad,
			fmt.Errorf("%w: unknown transport profile", ErrPeerTransportUnsupported))
	}
	budget := advertisement.GetBudget()
	if budget == nil {
		return config.TransportEffective{}, 0, transportReject(transportCodeBudget,
			fmt.Errorf("%w: receive budget missing", ErrPeerTransportUnsupported))
	}
	effective := config.TransportEffective{
		Profile:             profileName,
		WireName:            wireName,
		WireProfile:         profile,
		PacketBytes:         budget.GetPacketBytes(),
		PacketOverheadBytes: base.PacketOverheadBytes,
		EnvelopeBytes:       budget.GetEnvelopeBytes(),
		BulkConcurrency:     budget.GetBulkConcurrency(),
		ReservedBytes:       budget.GetReservedBytes(),
		TransferTimeout:     time.Duration(budget.GetTransferTimeoutMs()) * time.Millisecond,
	}
	if effective.EnvelopeBytes > math.MaxInt64 {
		return config.TransportEffective{}, 0, transportReject(transportCodeBudgetBad,
			fmt.Errorf("%w: receive envelope is not representable", ErrPeerTransportUnsupported))
	}
	if err := config.ValidateTransportEffective(effective); err != nil {
		return config.TransportEffective{}, 0, transportReject(transportCodeBudgetBad,
			fmt.Errorf("%w: invalid receive budget", ErrPeerTransportUnsupported))
	}
	return effective, advertisement.GetConnectionGeneration(), nil
}

// negotiateBudget intersects the local and advertised receive budgets field by
// field. A peer can only lower a local limit, and a relation-valid override may
// tighten one dimension without being silently raised back to a packaged
// profile. The result is an intersection rather than a named profile, so the
// profile label names only the base vocabulary.
func negotiateBudget(local, advertised config.TransportEffective) config.TransportEffective {
	if advertised.EnvelopeBytes == 0 {
		return local
	}
	strict := local
	if advertised.EnvelopeBytes < local.EnvelopeBytes {
		strict = advertised
	}
	strict.PacketBytes = min(local.PacketBytes, advertised.PacketBytes)
	strict.PacketOverheadBytes = min(local.PacketOverheadBytes, advertised.PacketOverheadBytes)
	strict.EnvelopeBytes = min(local.EnvelopeBytes, advertised.EnvelopeBytes)
	strict.BulkConcurrency = min(local.BulkConcurrency, advertised.BulkConcurrency)
	strict.ReservedBytes = min(local.ReservedBytes, advertised.ReservedBytes)
	strict.TransferTimeout = min(local.TransferTimeout, advertised.TransferTimeout)
	// Ordered-publisher settings are local: a receiver advertisement never
	// raises or lowers this publisher's queue bounds.
	strict.ReceiveMaximum = local.ReceiveMaximum
	strict.SendQueueDepth = local.SendQueueDepth
	strict.SendQueueBytes = local.SendQueueBytes
	strict.SendInFlight = local.SendInFlight
	strict.ResultLimitBytes = local.ResultLimitBytes
	strict.HTTPBodyBytes = local.HTTPBodyBytes
	return strict
}

// receiverRoute identifies one Access -> peer route whose receive limits are
// registered from an authenticated advertisement. Device routes are not
// host-scoped: the same authenticated receiver connection carries the same
// limits for every Host grant it holds.
type receiverRoute struct {
	kind     string // "host" or "device"
	hostID   string
	deviceID string
}

func (r receiverRoute) key() string {
	return r.kind + "\x00" + r.hostID + "\x00" + r.deviceID
}

func routeForTopic(topic string) (receiverRoute, bool) {
	parsed, err := ParseTopic(topic)
	if err != nil {
		return receiverRoute{}, false
	}
	switch parsed.Kind {
	case KindRequest, KindPresence:
		return receiverRoute{kind: "host", hostID: parsed.HostID}, true
	case KindDeviceEvent, KindDeviceNotice:
		return receiverRoute{kind: "device", deviceID: parsed.DeviceID}, true
	default:
		return receiverRoute{}, false
	}
}

// maxReceiversPerRoute bounds advertisements retained for one route.
const maxReceiversPerRoute = 64

type receiverEntry struct {
	connID     uint64
	generation uint64
	clientID   string
	budget     config.TransportEffective
}

// ReceiverBudgets registers authenticated receive advertisements per concrete
// receiver connection. One sibling connection reconnecting never clears
// another sibling's advertisement: entries are keyed by the broker-derived
// connection identity, and only an offline notification for that exact
// connection removes it. A shared destination route publishes at the minimum
// limit of every eligible connection.
type ReceiverBudgets struct {
	mu      sync.RWMutex
	entries map[string]map[uint64]receiverEntry
	current map[string]map[uint64]string
}

// NewReceiverBudgets creates an empty advertisement registry.
func NewReceiverBudgets() *ReceiverBudgets {
	return &ReceiverBudgets{
		entries: map[string]map[uint64]receiverEntry{},
		current: map[string]map[uint64]string{},
	}
}

// NoteConnection records one broker-derived receiver connection. A newer
// connection is tracked beside every other live sibling; only an offline event
// for the same connection invalidates its advertisement.
func (b *ReceiverBudgets) NoteConnection(route receiverRoute, connID uint64, clientID string, online bool) {
	if b == nil || connID == 0 {
		return
	}
	key := route.key()
	b.mu.Lock()
	defer b.mu.Unlock()
	if !online {
		connections, ok := b.current[key]
		if !ok || len(connections) == 0 {
			return
		}
		delete(connections, connID)
		if len(connections) > 0 {
			// A sibling receiver is still connected: only this exact
			// broker-derived connection loses its advertisement.
			if entries, ok := b.entries[key]; ok {
				delete(entries, connID)
				if len(entries) == 0 {
					delete(b.entries, key)
				}
			}
			return
		}
		// The last known connection for the route is gone. Device
		// advertisements are keyed by their advertised generation rather than
		// the broker connection, so the whole route is invalidated.
		delete(b.current, key)
		delete(b.entries, key)
		return
	}
	connections := b.current[key]
	if connections == nil {
		connections = map[uint64]string{}
		b.current[key] = connections
	}
	connections[connID] = clientID
}

// Register stores one authenticated advertisement. It returns false for a
// stale generation, an unknown connection or a superseded entry. A device
// advertisement is accepted only when its generation is one of the broker's
// current connections for that device; an old generation never refreshes a
// newer one.
func (b *ReceiverBudgets) Register(route receiverRoute, connID, generation uint64, budget config.TransportEffective) bool {
	if b == nil || generation == 0 {
		return false
	}
	key := route.key()
	entryKey := connID
	if entryKey == 0 {
		entryKey = generation
	}
	b.mu.Lock()
	defer b.mu.Unlock()
	if connections, ok := b.current[key]; ok && len(connections) > 0 && connID != 0 {
		if _, live := connections[connID]; !live {
			return false
		}
	}
	entries := b.entries[key]
	if entries == nil {
		entries = map[uint64]receiverEntry{}
		b.entries[key] = entries
	}
	if existing, ok := entries[entryKey]; ok && existing.generation >= generation {
		return false
	}
	// Bound one route's receivers. A Device may use a fresh local generation
	// per sibling connection and Access cannot observe which connection
	// published an advertisement, so old generations are pruned rather than
	// rejected: rejecting a lower sibling generation would drop a live
	// receiver's limits.
	if len(entries) >= maxReceiversPerRoute {
		var oldest uint64
		first := true
		for id := range entries {
			if first || id < oldest {
				oldest, first = id, false
			}
		}
		delete(entries, oldest)
	}
	clientID := ""
	if connections, ok := b.current[key]; ok {
		clientID = connections[connID]
	}
	entries[entryKey] = receiverEntry{connID: connID, generation: generation, clientID: clientID, budget: budget}
	return true
}

// Lookup returns the negotiated budget for one destination route, taking the
// strictest of every eligible connection when a route is shared.
func (b *ReceiverBudgets) Lookup(topic string) (config.TransportEffective, bool) {
	route, ok := routeForTopic(topic)
	if !ok {
		return config.TransportEffective{}, false
	}
	return b.minimum([]receiverRoute{route})
}

func (b *ReceiverBudgets) minimum(routes []receiverRoute) (config.TransportEffective, bool) {
	if b == nil {
		return config.TransportEffective{}, false
	}
	b.mu.RLock()
	defer b.mu.RUnlock()
	var selected config.TransportEffective
	found := false
	for _, route := range routes {
		for _, entry := range b.entries[route.key()] {
			if !found {
				selected = entry.budget
				found = true
				continue
			}
			selected = negotiateBudget(selected, entry.budget)
		}
	}
	return selected, found
}

// Clear removes every advertisement for one route (for example when metadata
// is missing or invalid).
func (b *ReceiverBudgets) Clear(route receiverRoute) {
	if b == nil {
		return
	}
	b.mu.Lock()
	delete(b.entries, route.key())
	b.mu.Unlock()
}

// FenceDevice removes every advertisement registered for a device. A grant or
// app-session revocation fences all of its sibling receivers at once.
func (b *ReceiverBudgets) FenceDevice(deviceID string) {
	if b == nil || deviceID == "" {
		return
	}
	b.mu.Lock()
	defer b.mu.Unlock()
	route := receiverRoute{kind: "device", deviceID: deviceID}
	delete(b.entries, route.key())
	delete(b.current, route.key())
}

// Reset drops every advertisement; a new Access MQTT connection must receive
// fresh readiness proofs.
func (b *ReceiverBudgets) Reset() {
	if b == nil {
		return
	}
	b.mu.Lock()
	b.entries = map[string]map[uint64]receiverEntry{}
	b.current = map[string]map[uint64]string{}
	b.mu.Unlock()
}

// Count returns the number of registered receiver connections, for operational
// assertions.
func (b *ReceiverBudgets) Count() int {
	if b == nil {
		return 0
	}
	b.mu.RLock()
	defer b.mu.RUnlock()
	count := 0
	for _, entries := range b.entries {
		count += len(entries)
	}
	return count
}

// Connections returns the number of live broker connections tracked for one
// route. It is the aggregate view used for presence diagnostics.
func (b *ReceiverBudgets) Connections(route receiverRoute) int {
	if b == nil {
		return 0
	}
	b.mu.RLock()
	defer b.mu.RUnlock()
	return len(b.current[route.key()])
}
