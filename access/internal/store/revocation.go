package store

import (
	"strconv"
	"strings"
)

// ConnectionRef is the Broker-issued identity of one accepted connection: a
// broker-process epoch plus the connection instance within that process.
//
// The broker package aliases this type so the transport registry, Access
// presence and the durable revocation scope share one representation.
type ConnectionRef struct {
	Epoch string
	ID    uint64
}

// String renders the wire form <broker-epoch>:<connection-instance>.
func (r ConnectionRef) String() string {
	if r.Epoch == "" {
		return ""
	}
	return r.Epoch + ":" + strconv.FormatUint(r.ID, 10)
}

// Valid reports whether the reference names a broker epoch and connection.
func (r ConnectionRef) Valid() bool {
	return r.Epoch != "" && r.ID != 0 && !strings.ContainsAny(r.Epoch, ":/ \t\r\n")
}

// ParseConnectionRef parses the reserved connection reference property, the
// snapshot `ref` field and the durable receipt columns.
func ParseConnectionRef(value string) (ConnectionRef, bool) {
	epoch, rawID, ok := strings.Cut(strings.TrimSpace(value), ":")
	if !ok || epoch == "" || rawID == "" || strings.ContainsAny(epoch, "/ \t\r\n") {
		return ConnectionRef{}, false
	}
	id, err := strconv.ParseUint(rawID, 10, 64)
	if err != nil || id == 0 {
		return ConnectionRef{}, false
	}
	return ConnectionRef{Epoch: epoch, ID: id}, true
}

// ScopeConnection is one admitted Broker connection frozen into a revocation
// scope. ClientID is the actual broker client id, which may carry a bounded
// suffix for a second device connection.
type ScopeConnection struct {
	Ref      ConnectionRef
	ClientID string
}

// Valid reports whether the frozen connection can be targeted by a scoped
// control operation.
func (c ScopeConnection) Valid() bool {
	return c.Ref.Valid() && c.ClientID != ""
}

// ScopeGrant is one frozen Host/device grant authorization. It records the
// grant generation and the post-commit revocation generation so a delayed
// retry fences exactly the authorization it was created for.
type ScopeGrant struct {
	HostID                     string
	DeviceID                   string
	EnrollmentGeneration       uint64
	GrantGeneration            uint64
	RevocationGeneration       uint64
	DeviceRevocationGeneration uint64
	// Connections are the admitted device connections frozen for this grant.
	Connections []ScopeConnection
}

// Revision is the broker policy generation for this frozen grant.
func (g ScopeGrant) Revision() uint64 {
	return g.DeviceRevocationGeneration<<40 | g.RevocationGeneration<<20 | g.GrantGeneration
}

// RevocationScope is one frozen revocation operation. It captures the exact
// authorization generations and, when live, the admitted Broker connection
// instances observed for that scope. Enforcement never re-reads a newer
// generation from the durable grant row.
type RevocationScope struct {
	HostID     string
	DeviceID   string
	DeviceWide bool
	// EnrollmentGeneration is the managed Host enrollment the frozen scope
	// belongs to; zero is the Standalone generation.
	EnrollmentGeneration uint64
	// Grants holds every frozen grant authorization of the scope. A single
	// grant scope has one entry; a device-wide or Host-disable scope has one
	// per revoked Host/device pair. Each grant carries the admitted device
	// connections observed while the frozen scope was still current.
	Grants []ScopeGrant
	// HostConnection is the admitted Host connection for a Host-disable scope.
	HostConnection *ScopeConnection
}

// Frozen returns a deep copy so a retry cannot mutate the stored scope.
func (s RevocationScope) Frozen() RevocationScope {
	out := s
	if len(s.Grants) > 0 {
		out.Grants = make([]ScopeGrant, len(s.Grants))
		for i, grant := range s.Grants {
			out.Grants[i] = grant
			if len(grant.Connections) > 0 {
				out.Grants[i].Connections = append([]ScopeConnection(nil), grant.Connections...)
			}
		}
	}
	if s.HostConnection != nil {
		host := *s.HostConnection
		out.HostConnection = &host
	}
	return out
}

// EnrollmentReplaced reports whether the current enrollment generation is
// newer than the frozen one. In that case the old scope was already replaced
// and no current connection belongs to it.
func (s RevocationScope) EnrollmentReplaced(current uint64) bool {
	return current > s.EnrollmentGeneration
}

// EnforcementReceipt reports which frozen connection instances a scoped
// operation confirmed. An absent or already replaced connection counts as
// enforced for the old scope, so an empty Applied set is still success.
type EnforcementReceipt struct {
	Scope   RevocationScope
	Applied []ScopeConnection
	Absent  bool
}

// Primary returns the first applied connection, the evidence persisted in a
// durable receipt row. A scope with several frozen connections keeps the
// remaining evidence in the receipt map and broker audit trail.
func (r EnforcementReceipt) Primary() *ConnectionRef {
	for _, connection := range r.Applied {
		if !connection.Ref.Valid() {
			continue
		}
		ref := connection.Ref
		return &ref
	}
	return nil
}
