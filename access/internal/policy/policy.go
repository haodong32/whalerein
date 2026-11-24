// Package policy projects committed grants into the local policy a Mosquitto
// connection is admitted with. It is the only place that translates WhaleRein
// grant state into broker topic permissions, so the broker contract stays
// explicit and testable without a running broker.
package policy

import (
	"fmt"
	"sort"
	"strings"

	"github.com/whalerein/whalerein/access/internal/store"
)

// Principal kinds understood by the broker plugin.
const (
	KindAccess = "access"
	KindHost   = "host"
	KindDevice = "device"
	KindProbe  = "probe"
)

// Topic grammar shared with the wire contract.
const (
	topicRoot     = "whalerein/v1/h/"
	presenceRoot  = "whalerein/v1/presence/device/"
	controlPrefix = "$CONTROL/whalerein/v1"
)

// Entry is one local ACL decision for the connection.
type Entry struct {
	Access   string
	Revision uint64
	HostID   string
	Filter   string
}

// Connection is the complete policy installed for one MQTT connection.
type Connection struct {
	SessionID        string
	SessionRevision  int64
	SessionExpiresAt int64
	Kind             string
	HostID           string
	DeviceID         string
	Revision         uint64
	Will             string
	Entries          []Entry
}

// Revision packs the durable generation counters into one monotonic value per
// Host/device scope. Device-wide revocation rises in the high component,
// Host-scoped revocation in the middle and re-pairing in the low component, so
// comparing packed revisions never mistakes an older grant for a newer one.
func Revision(grant store.AdmissionGrant) uint64 {
	return grant.DeviceRevocationGeneration<<40 | grant.RevocationGeneration<<20 | grant.GrantGeneration
}

// Build returns the complete policy for one authenticated principal. The
// Access identity policy is compiled into the broker plugin; Build returns an
// empty policy for it.
func Build(kind, hostID, deviceID string, grants []store.AdmissionGrant) Connection {
	connection := Connection{Kind: kind, HostID: hostID, DeviceID: deviceID}
	switch kind {
	case KindHost:
		connection.Entries = []Entry{
			{Access: "subscribe", Filter: topicRoot + hostID + "/request"},
			{Access: "read", Filter: topicRoot + hostID + "/request"},
			{Access: "subscribe", Filter: topicRoot + hostID + "/presence"},
			{Access: "read", Filter: topicRoot + hostID + "/presence"},
			{Access: "write", Filter: topicRoot + hostID + "/result"},
			{Access: "write", Filter: topicRoot + hostID + "/event"},
		}
		// Direct QoS 0 previews are published by the Host for each Device it
		// is currently authorized for. The retired h/{host}/live relay entry
		// is never projected again.
		for _, grant := range grants {
			if !grant.Active || grant.HostID != hostID || grant.DeviceID == "" {
				continue
			}
			revision := Revision(grant)
			connection.Entries = append(connection.Entries,
				Entry{Access: "write", Revision: revision, HostID: hostID,
					Filter: topicRoot + hostID + "/d/" + grant.DeviceID + "/live"})
			if revision > connection.Revision {
				connection.Revision = revision
			}
		}
	case KindDevice:
		connection.Will = presenceRoot + deviceID
		seen := map[string]struct{}{}
		for _, grant := range grants {
			if !grant.Active || grant.HostID == "" {
				continue
			}
			if _, ok := seen[grant.HostID]; ok {
				continue
			}
			seen[grant.HostID] = struct{}{}
			revision := Revision(grant)
			eventTopic := topicRoot + grant.HostID + "/d/" + deviceID + "/event"
			noticeTopic := topicRoot + grant.HostID + "/d/" + deviceID + "/notice"
			liveTopic := topicRoot + grant.HostID + "/d/" + deviceID + "/live"
			queryTopic := topicRoot + grant.HostID + "/d/" + deviceID + "/query"
			replyTopic := topicRoot + grant.HostID + "/d/" + deviceID + "/reply"
			connection.Entries = append(connection.Entries,
				Entry{Access: "subscribe", Revision: revision, HostID: grant.HostID, Filter: eventTopic},
				Entry{Access: "read", Revision: revision, HostID: grant.HostID, Filter: eventTopic},
				Entry{Access: "subscribe", Revision: revision, HostID: grant.HostID, Filter: noticeTopic},
				Entry{Access: "read", Revision: revision, HostID: grant.HostID, Filter: noticeTopic},
				Entry{Access: "subscribe", Revision: revision, HostID: grant.HostID, Filter: liveTopic},
				Entry{Access: "read", Revision: revision, HostID: grant.HostID, Filter: liveTopic},
				Entry{Access: "subscribe", Revision: revision, HostID: grant.HostID, Filter: replyTopic},
				Entry{Access: "read", Revision: revision, HostID: grant.HostID, Filter: replyTopic},
				Entry{Access: "write", Revision: revision, HostID: grant.HostID, Filter: queryTopic},
				Entry{Access: "write", Revision: revision, HostID: grant.HostID, Filter: topicRoot + grant.HostID + "/d/" + deviceID + "/ready"},
			)
			if revision > connection.Revision {
				connection.Revision = revision
			}
		}
		connection.Entries = append(connection.Entries,
			Entry{Access: "write", Revision: connection.Revision, Filter: presenceRoot + deviceID})
	case KindProbe:
		// Probe clients prove credential acceptance only and receive no topic.
	case KindAccess:
		// The broker plugin owns the internal Access policy.
	}
	sort.Slice(connection.Entries, func(i, j int) bool {
		left, right := connection.Entries[i], connection.Entries[j]
		if left.Access != right.Access {
			return left.Access < right.Access
		}
		if left.HostID != right.HostID {
			return left.HostID < right.HostID
		}
		return left.Filter < right.Filter
	})
	return connection
}

// WillAllowed reports whether a CONNECT Will topic is inside the policy.
func (c Connection) WillAllowed(topic string) bool {
	if topic == "" {
		return true
	}
	if c.Kind != KindDevice || c.Will == "" || topic != c.Will {
		return false
	}
	return true
}

// Format renders the versioned broker policy response.
func Format(c Connection) []byte {
	var builder strings.Builder
	builder.WriteString("whalerein/1\nok\n")
	fmt.Fprintf(&builder, "kind %s\n", c.Kind)
	if c.HostID != "" {
		fmt.Fprintf(&builder, "host %s\n", c.HostID)
	}
	if c.DeviceID != "" {
		fmt.Fprintf(&builder, "device %s\n", c.DeviceID)
	}
	fmt.Fprintf(&builder, "revision %d\nready 1\n", c.Revision)
	if c.SessionID != "" {
		fmt.Fprintf(&builder, "session %s %d %d\n", c.SessionID, c.SessionRevision, c.SessionExpiresAt)
	}
	if c.Will != "" {
		fmt.Fprintf(&builder, "will %s\n", c.Will)
	}
	for _, entry := range c.Entries {
		host := entry.HostID
		if host == "" {
			host = "-"
		}
		fmt.Fprintf(&builder, "acl %s %d %s %s\n", entry.Access, entry.Revision, host, entry.Filter)
	}
	return []byte(builder.String())
}

// ControlMessages renders the policy management operations that reconcile one
// connection: one set request, or a removal when the connection has no policy.
func ControlMessages(c Connection, clientID string, ttlMillis int) []string {
	fields := []string{
		fmt.Sprintf("client %s", clientID),
		fmt.Sprintf("kind %s", c.Kind),
		fmt.Sprintf("device %s", c.DeviceID),
		fmt.Sprintf("host %s", c.HostID),
		fmt.Sprintf("revision %d", c.Revision),
		fmt.Sprintf("ttl %d", ttlMillis),
	}
	for _, entry := range c.Entries {
		host := entry.HostID
		if host == "" {
			host = "-"
		}
		fields = append(fields, fmt.Sprintf("acl %s %d %s %s", entry.Access, entry.Revision, host, entry.Filter))
	}
	return fields
}
