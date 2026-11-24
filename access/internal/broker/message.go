package broker

import (
	"strconv"
	"strings"
)

// Reserved MQTT 5 User Property names. The pinned broker stamps
// PropertyConnection on every external ingress PUBLISH and checks
// PropertyTarget on directed deliveries. Access only reads the ingress stamp
// and sets the target on query replies.
const (
	PropertyConnection = "whalerein-connection"
	PropertyTarget     = "whalerein-target"
)

// UserProperty is one MQTT 5 User Property pair.
type UserProperty struct {
	Key   string
	Value string
}

// MessageProperties is the MQTT 5 property subset the relay contract uses.
// Every field is optional; a zero value omits the property.
type MessageProperties struct {
	CorrelationData []byte
	ResponseTopic   string
	MessageExpiry   *uint32
	UserProperties  []UserProperty
}

// Get returns the first User Property value for key.
func (p MessageProperties) Get(key string) string {
	for _, property := range p.UserProperties {
		if property.Key == key {
			return property.Value
		}
	}
	return ""
}

// Has reports whether any User Property with key is present.
func (p MessageProperties) Has(key string) bool {
	for _, property := range p.UserProperties {
		if property.Key == key {
			return true
		}
	}
	return false
}

// WithUserProperty returns a copy with one User Property appended.
func (p MessageProperties) WithUserProperty(key, value string) MessageProperties {
	properties := make([]UserProperty, 0, len(p.UserProperties)+1)
	properties = append(properties, p.UserProperties...)
	properties = append(properties, UserProperty{Key: key, Value: value})
	p.UserProperties = properties
	return p
}

// Message is one complete MQTT 5 PUBLISH. The same shape is used for ingress
// dispatch and outbound publication so properties never get dropped between
// the connector and the dispatcher.
type Message struct {
	Topic      string
	Payload    []byte
	QoS        byte
	Retain     bool
	Properties MessageProperties
}

// ConnectionRef is the Broker-issued identity of one accepted connection:
// a broker-process epoch plus the connection instance within that process.
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

// ParseConnectionRef parses the reserved connection reference property and the
// snapshot `ref` field.
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
