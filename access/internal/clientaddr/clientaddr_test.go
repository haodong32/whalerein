package clientaddr

import (
	"net/http"
	"net/netip"
	"testing"
)

func TestParseTrustedProxies(t *testing.T) {
	proxies, err := ParseTrustedProxies([]string{"172.18.0.0/16", " 127.0.0.1/32 ", "172.18.0.0/16"})
	if err != nil {
		t.Fatal(err)
	}
	if len(proxies) != 2 {
		t.Fatalf("proxies = %v", proxies)
	}
	for _, value := range []string{"", "not-a-cidr", "172.18.0.0", "0.0.0.0/0", "::/0"} {
		if _, err := ParseTrustedProxies([]string{value}); err == nil {
			t.Fatalf("accepted trusted proxy %q", value)
		}
	}
}

func TestResolverSelectsTheTrustedHeaderOrPeer(t *testing.T) {
	resolver, err := NewResolver([]string{"172.18.0.0/16", "::1/128"})
	if err != nil {
		t.Fatal(err)
	}
	for _, test := range []struct {
		name    string
		remote  string
		header  string
		want    string
		source  Source
		private bool
	}{
		{name: "public peer", remote: "198.51.100.9:40123", source: SourceDirect},
		{name: "public peer ignores the header", remote: "198.51.100.9:40123", header: "203.0.113.7", source: SourceDirect},
		{name: "trusted ingress forwards the client", remote: "172.18.0.2:443", header: "203.0.113.7", want: "203.0.113.7", source: SourceForwarded},
		{name: "trusted ingress with a private client", remote: "172.18.0.2:443", header: "10.1.2.3", want: "10.1.2.3", source: SourceForwarded},
		{name: "trusted ingress without a header", remote: "172.18.0.2:443", source: SourceUnavailable},
		{name: "trusted ingress with a list", remote: "172.18.0.2:443", header: "203.0.113.7, 10.0.0.1", source: SourceUnavailable},
		{name: "trusted ingress with a port", remote: "172.18.0.2:443", header: "203.0.113.7:443", source: SourceUnavailable},
		{name: "trusted ingress with a zone", remote: "172.18.0.2:443", header: "fe80::1%eth0", source: SourceUnavailable},
		{name: "trusted ingress with an unspecified address", remote: "172.18.0.2:443", header: "0.0.0.0", source: SourceUnavailable},
		{name: "loopback peer", remote: "[::1]:443", header: "2001:db8::7", want: "2001:db8::7", source: SourceForwarded},
		{name: "mapped peer resolves to IPv4", remote: "[::ffff:172.18.0.2]:443", header: "203.0.113.7", want: "203.0.113.7", source: SourceForwarded},
		{name: "unknown peer address", remote: "not-an-address", source: SourceUnavailable},
		{name: "empty peer address", remote: "", source: SourceUnavailable},
	} {
		t.Run(test.name, func(t *testing.T) {
			header := http.Header{}
			if test.header != "" {
				header.Set(ClientHeader, test.header)
			}
			identity := resolver.Resolve(test.remote, header)
			if identity.Source != test.source {
				t.Fatalf("source = %q, want %q", identity.Source, test.source)
			}
			if test.source == SourceUnavailable {
				if identity.Address.IsValid() {
					t.Fatalf("unavailable identity has address %v", identity.Address)
				}
				return
			}
			if test.source == SourceDirect {
				peer, ok := parsePeer(test.remote)
				if !ok || identity.Address != peer {
					t.Fatalf("direct address = %v, want peer %v", identity.Address, peer)
				}
				return
			}
			want, err := netip.ParseAddr(test.want)
			if err != nil {
				t.Fatal(err)
			}
			if identity.Address != want {
				t.Fatalf("address = %v, want %v", identity.Address, want)
			}
		})
	}
}

func TestResolverWithoutTrustedProxiesIsDirect(t *testing.T) {
	resolver, err := NewResolver(nil)
	if err != nil {
		t.Fatal(err)
	}
	identity := resolver.Resolve("198.51.100.9:40123", http.Header{ClientHeader: []string{"203.0.113.7"}})
	if identity.Source != SourceDirect || identity.Address.String() != "198.51.100.9" {
		t.Fatalf("identity = %+v", identity)
	}
}

func TestNilResolverUsesThePeer(t *testing.T) {
	var resolver *Resolver
	identity := resolver.Resolve("198.51.100.9:40123", nil)
	if identity.Source != SourceDirect || identity.Address.String() != "198.51.100.9" {
		t.Fatalf("identity = %+v", identity)
	}
}
