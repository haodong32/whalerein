// Package clientaddr resolves the public client address for access records.
//
// The public ingress is the only authority for the address. It writes the
// single-valued ClientHeader after overwriting any inbound value. Access reads
// that header only when the TCP peer is inside an explicitly configured
// trusted proxy prefix; a direct deployment uses the peer address instead.
package clientaddr

import (
	"fmt"
	"net"
	"net/http"
	"net/netip"
	"strings"
)

// ClientHeader is fixed by the ingress contract. Access never reads
// X-Forwarded-For or X-Real-IP.
const ClientHeader = "X-Whalerein-Client-IP"

// Source describes how an Identity was selected. It is safe to log.
type Source string

const (
	// SourceForwarded means a trusted peer supplied the dedicated header.
	SourceForwarded Source = "forwarded"
	// SourceDirect means no trusted proxy matched, so the peer is the client.
	SourceDirect Source = "direct"
	// SourceUnavailable means a trusted peer did not supply a usable header.
	SourceUnavailable Source = "unavailable"
)

// Identity is the resolved client address and where it came from.
//
// Address is invalid when Source is SourceUnavailable. The address is kept in
// memory only; logging uses the windowed reference from package reference.go.
type Identity struct {
	Address netip.Addr
	Source  Source
}

// Resolver validates the trusted proxy configuration and resolves identities.
type Resolver struct {
	trusted []netip.Prefix
}

// ParseTrustedProxies validates CIDR strings and drops duplicate entries.
func ParseTrustedProxies(values []string) ([]netip.Prefix, error) {
	prefixes := make([]netip.Prefix, 0, len(values))
	seen := make(map[netip.Prefix]bool, len(values))
	for _, value := range values {
		trimmed := strings.TrimSpace(value)
		if trimmed == "" {
			return nil, fmt.Errorf("trusted proxy CIDR is empty")
		}
		prefix, err := netip.ParsePrefix(trimmed)
		if err != nil {
			return nil, fmt.Errorf("trusted proxy %q is not a CIDR: %w", trimmed, err)
		}
		if prefix.Bits() == 0 {
			return nil, fmt.Errorf("trusted proxy %q must not trust the whole address space", trimmed)
		}
		prefix = prefix.Masked()
		if seen[prefix] {
			continue
		}
		seen[prefix] = true
		prefixes = append(prefixes, prefix)
	}
	return prefixes, nil
}

// NewResolver creates a resolver from trusted proxy CIDRs. An empty list is a
// direct deployment: the dedicated header is ignored.
func NewResolver(trusted []string) (*Resolver, error) {
	prefixes, err := ParseTrustedProxies(trusted)
	if err != nil {
		return nil, err
	}
	return &Resolver{trusted: prefixes}, nil
}

// Resolve returns the client identity for one request.
func (r *Resolver) Resolve(remoteAddr string, header http.Header) Identity {
	peer, ok := parsePeer(remoteAddr)
	if !ok {
		return Identity{Source: SourceUnavailable}
	}
	if r == nil || len(r.trusted) == 0 || !r.contains(peer) {
		return Identity{Address: peer, Source: SourceDirect}
	}
	client, ok := parseClientHeader(header.Get(ClientHeader))
	if !ok {
		return Identity{Source: SourceUnavailable}
	}
	return Identity{Address: client, Source: SourceForwarded}
}

func (r *Resolver) contains(peer netip.Addr) bool {
	for _, prefix := range r.trusted {
		if prefix.Contains(peer) {
			return true
		}
	}
	return false
}

func parsePeer(remoteAddr string) (netip.Addr, bool) {
	if strings.TrimSpace(remoteAddr) == "" {
		return netip.Addr{}, false
	}
	host, _, err := net.SplitHostPort(remoteAddr)
	if err != nil {
		host = remoteAddr
	}
	host = strings.TrimSpace(host)
	if strings.HasPrefix(host, "[") && strings.HasSuffix(host, "]") {
		host = host[1 : len(host)-1]
	}
	return parseUnicast(host)
}

func parseClientHeader(value string) (netip.Addr, bool) {
	value = strings.TrimSpace(value)
	if value == "" || strings.ContainsAny(value, " \t,;") {
		return netip.Addr{}, false
	}
	return parseUnicast(value)
}

func parseUnicast(value string) (netip.Addr, bool) {
	addr, err := netip.ParseAddr(value)
	if err != nil || addr.Zone() != "" {
		return netip.Addr{}, false
	}
	addr = addr.Unmap()
	if !addr.IsValid() || addr.IsUnspecified() || addr.IsMulticast() {
		return netip.Addr{}, false
	}
	return addr, true
}
