package broker

import (
	"errors"
	"fmt"
	"math"
	"strings"
	"sync"
	"time"

	"github.com/whalerein/whalerein/access/internal/config"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// Stable transport rejection codes. They are the only labels emitted to logs
// and metrics; they never carry a topic, peer identity or payload.
const (
	transportCodePacket     = "frame_packet"
	transportCodeHeader     = "frame_header"
	transportCodeWidth      = "frame_width"
	transportCodeGeometry   = "frame_geometry"
	transportCodeEnvelope   = "envelope_budget"
	transportCodeFragments  = "fragment_count"
	transportCodeReassembly = "reassembly_budget"
	transportCodeIntegrity  = "frame_integrity"
	transportCodeTopic      = "frame_topic"
	transportCodeReceipt    = "receipt"
	transportCodeAdmission  = "bulk_admission"
	transportCodeBudget     = "budget_missing"
	transportCodeBudgetBad  = "budget_invalid"
)

var transportCodes = map[string]struct{}{
	transportCodePacket: {}, transportCodeHeader: {}, transportCodeWidth: {},
	transportCodeGeometry: {}, transportCodeEnvelope: {}, transportCodeFragments: {},
	transportCodeReassembly: {}, transportCodeIntegrity: {}, transportCodeTopic: {},
	transportCodeReceipt: {}, transportCodeAdmission: {}, transportCodeBudget: {},
	transportCodeBudgetBad: {},
}

// TransportError is a rejected frame or admission decision with a stable code.
// It never includes peer identifiers or payload content.
type TransportError struct {
	Code string
	err  error
}

func (e *TransportError) Error() string {
	if e.err == nil {
		return "logical transport rejected"
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
			fmt.Errorf("%w: receive advertisement missing", ErrPeerBudgetMissing))
	}
	if advertisement.GetConnectionGeneration() == 0 {
		return config.TransportEffective{}, 0, transportReject(transportCodeBudgetBad,
			fmt.Errorf("%w: receive advertisement generation missing", ErrPeerBudgetInvalid))
	}
	profile := advertisement.GetProfile()
	wireName, known := whalereinv1.TransportProfile_name[int32(profile)]
	if !known || profile == whalereinv1.TransportProfile_TRANSPORT_PROFILE_UNSPECIFIED {
		return config.TransportEffective{}, 0, transportReject(transportCodeBudgetBad,
			fmt.Errorf("%w: unknown transport profile", ErrPeerBudgetInvalid))
	}
	profileName, ok := config.ProfileForWireName(wireName)
	if !ok {
		return config.TransportEffective{}, 0, transportReject(transportCodeBudgetBad,
			fmt.Errorf("%w: unknown transport profile", ErrPeerBudgetInvalid))
	}
	base, ok := whalereinv1.TransportProfileValuesFor(profileName)
	if !ok {
		return config.TransportEffective{}, 0, transportReject(transportCodeBudgetBad,
			fmt.Errorf("%w: unknown transport profile", ErrPeerBudgetInvalid))
	}
	budget := advertisement.GetBudget()
	if budget == nil {
		return config.TransportEffective{}, 0, transportReject(transportCodeBudget,
			fmt.Errorf("%w: receive budget missing", ErrPeerBudgetMissing))
	}
	effective := config.TransportEffective{
		Profile:              profileName,
		WireName:             wireName,
		WireProfile:          profile,
		PacketBytes:          budget.GetPacketBytes(),
		PacketOverheadBytes:  base.PacketOverheadBytes,
		FragmentPayloadBytes: budget.GetFragmentPayloadBytes(),
		FragmentCount:        budget.GetFragmentCount(),
		EnvelopeBytes:        budget.GetEnvelopeBytes(),
		BulkConcurrency:      budget.GetBulkConcurrency(),
		ReservedBytes:        budget.GetReservedBytes(),
		FragmentIdleTimeout:  time.Duration(budget.GetFragmentIdleTimeoutMs()) * time.Millisecond,
		TransferTimeout:      time.Duration(budget.GetTransferTimeoutMs()) * time.Millisecond,
	}
	// A TransportFrame total is a uint32, so a larger logical envelope could
	// never be transferred even if its fragment count were representable.
	if effective.EnvelopeBytes > math.MaxUint32 {
		return config.TransportEffective{}, 0, transportReject(transportCodeBudgetBad,
			fmt.Errorf("%w: receive envelope is not representable", ErrPeerBudgetInvalid))
	}
	if err := config.ValidateTransportEffective(effective); err != nil {
		return config.TransportEffective{}, 0, transportReject(transportCodeBudgetBad,
			fmt.Errorf("%w: invalid receive budget", ErrPeerBudgetInvalid))
	}
	return effective, advertisement.GetConnectionGeneration(), nil
}

// negotiateBudget intersects the local and advertised receive budgets field by
// field. A peer can only lower a local limit, and a relation-valid override may
// tighten one dimension without being silently raised back to a packaged
// profile. The result is an intersection rather than a named profile, so the
// profile label names only the base vocabulary. Geometry that no longer fits
// makes the oversized send fail explicitly in sendTo; it is never clamped.
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
	strict.FragmentPayloadBytes = min(local.FragmentPayloadBytes, advertised.FragmentPayloadBytes)
	strict.FragmentCount = min(local.FragmentCount, advertised.FragmentCount)
	strict.EnvelopeBytes = min(local.EnvelopeBytes, advertised.EnvelopeBytes)
	strict.BulkConcurrency = min(local.BulkConcurrency, advertised.BulkConcurrency)
	strict.ReservedBytes = min(local.ReservedBytes, advertised.ReservedBytes)
	strict.FragmentIdleTimeout = min(local.FragmentIdleTimeout, advertised.FragmentIdleTimeout)
	strict.TransferTimeout = min(local.TransferTimeout, advertised.TransferTimeout)
	return strict
}

// receiverRoute identifies one Access -> peer route whose receive limits are
// registered from an authenticated advertisement.
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
	case KindDeviceEvent:
		return receiverRoute{kind: "device", hostID: parsed.HostID, deviceID: parsed.DeviceID}, true
	default:
		return receiverRoute{}, false
	}
}

type receiverEntry struct {
	connID     uint64
	generation uint64
	budget     config.TransportEffective
}

// ReceiverBudgets registers authenticated receive advertisements per route and
// broker-derived connection. It is deliberately outside the request forwarding
// loop: forwarding reads a snapshot, never waits on advertisements.
type ReceiverBudgets struct {
	mu      sync.RWMutex
	entries map[string]receiverEntry
	current map[string]uint64
}

// NewReceiverBudgets creates an empty advertisement registry.
func NewReceiverBudgets() *ReceiverBudgets {
	return &ReceiverBudgets{
		entries: map[string]receiverEntry{},
		current: map[string]uint64{},
	}
}

// NoteConnection records the broker-derived current connection for a route
// prefix. A newer connection invalidates the prior advertisement so a delayed
// readiness proof cannot refresh it.
func (b *ReceiverBudgets) NoteConnection(route receiverRoute, connID uint64, online bool) {
	if b == nil {
		return
	}
	key := route.key()
	b.mu.Lock()
	defer b.mu.Unlock()
	if route.kind == "device" && route.hostID == "" {
		// A broker device notification is not host-scoped. Invalidate every
		// route for that device; a reconnect must re-advertise.
		for existing := range b.entries {
			if strings.Contains(existing, "\x00"+route.deviceID) {
				delete(b.entries, existing)
				delete(b.current, existing)
			}
		}
		return
	}
	if !online {
		delete(b.entries, key)
		delete(b.current, key)
		return
	}
	if previous, ok := b.current[key]; ok && previous != 0 && connID != 0 && previous != connID {
		delete(b.entries, key)
	}
	b.current[key] = connID
}

// Register stores one authenticated advertisement. It returns false for a
// stale generation, a superseded connection or an advertisement that does not
// match the broker-derived current connection.
func (b *ReceiverBudgets) Register(route receiverRoute, connID, generation uint64, budget config.TransportEffective) bool {
	if b == nil {
		return false
	}
	key := route.key()
	b.mu.Lock()
	defer b.mu.Unlock()
	if current, ok := b.current[key]; ok && current != 0 && connID != current {
		return false
	}
	if existing, ok := b.entries[key]; ok {
		if existing.connID == connID && existing.generation >= generation {
			return false
		}
		// An unknown or superseded connection can never overwrite a newer
		// broker-derived connection.
		if existing.connID != 0 && (connID == 0 || existing.connID > connID) {
			return false
		}
	}
	b.entries[key] = receiverEntry{connID: connID, generation: generation, budget: budget}
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
		entry, ok := b.entries[route.key()]
		if !ok {
			continue
		}
		if !found {
			selected = entry.budget
			found = true
			continue
		}
		selected = negotiateBudget(selected, entry.budget)
	}
	return selected, found
}

// Clear removes the advertisement for one route (for example when metadata is
// missing or invalid).
func (b *ReceiverBudgets) Clear(route receiverRoute) {
	if b == nil {
		return
	}
	b.mu.Lock()
	delete(b.entries, route.key())
	b.mu.Unlock()
}

// Reset drops every advertisement; a new Access MQTT connection must receive
// fresh readiness proofs.
func (b *ReceiverBudgets) Reset() {
	if b == nil {
		return
	}
	b.mu.Lock()
	b.entries = map[string]receiverEntry{}
	b.current = map[string]uint64{}
	b.mu.Unlock()
}

// Count returns the number of registered routes, for operational assertions.
func (b *ReceiverBudgets) Count() int {
	if b == nil {
		return 0
	}
	b.mu.RLock()
	defer b.mu.RUnlock()
	return len(b.entries)
}
