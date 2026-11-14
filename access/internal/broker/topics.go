package broker

import (
	"errors"
	"strings"
)

// MQTT topics and filters from proto/v1/WIRE.md.
const (
	topicRoot    = "whalerein/v1/h/"
	presenceRoot = "whalerein/v1/presence/device/"

	// ResultFilter receives HostResult messages from every host.
	ResultFilter = "whalerein/v1/h/+/result"
	// EventFilter receives event envelopes from every host.
	EventFilter     = "whalerein/v1/h/+/event"
	DeviceAckFilter = "whalerein/v1/h/+/d/+/ack"
	// DeviceWillFilter receives non-retained MQTT last wills published by the
	// broker for disconnected device connections.
	DeviceWillFilter = presenceRoot + "+"
)

// TopicKind is one of the four wire topic shapes.
type TopicKind string

// Wire topic kinds.
const (
	KindRequest     TopicKind = "request"
	KindResult      TopicKind = "result"
	KindEvent       TopicKind = "event"
	KindDeviceEvent TopicKind = "device_event"
	KindDeviceAck   TopicKind = "device_ack"
	KindPresence    TopicKind = "presence"
)

// ErrInvalidTopic is returned for topics outside the four wire templates.
var ErrInvalidTopic = errors.New("invalid topic")

// Topic is a parsed wire topic.
type Topic struct {
	HostID   string
	DeviceID string
	Kind     TopicKind
}

func validID(value string) bool {
	return value != "" && !strings.ContainsAny(value, "/+#\x00")
}

// RequestTopic is the Access -> host request topic.
func RequestTopic(hostID string) string { return topicRoot + hostID + "/request" }

// ResultTopic is the host -> Access result topic.
func ResultTopic(hostID string) string { return topicRoot + hostID + "/result" }

// EventTopic is the host -> Access event topic.
func EventTopic(hostID string) string { return topicRoot + hostID + "/event" }

// DeviceEventTopic is the Access -> device event topic.
func DeviceEventTopic(hostID, deviceID string) string {
	return topicRoot + hostID + "/d/" + deviceID + "/event"
}

func DeviceAckTopic(hostID, deviceID string) string {
	return topicRoot + hostID + "/d/" + deviceID + "/ack"
}

// PresenceTopic is the Access -> host device-presence topic.
func PresenceTopic(hostID string) string { return topicRoot + hostID + "/presence" }

// DeviceWillTopic is the device-scoped MQTT last-will topic. It carries no
// host scope because one device may be authorized by more than one host.
func DeviceWillTopic(deviceID string) string { return presenceRoot + deviceID }

// ParseDeviceWillTopic parses a concrete device last-will topic. Broadcast
// filters and wildcard characters are rejected.
func ParseDeviceWillTopic(topic string) (string, bool) {
	if !strings.HasPrefix(topic, presenceRoot) {
		return "", false
	}
	deviceID := strings.TrimPrefix(topic, presenceRoot)
	if !validID(deviceID) || strings.Contains(deviceID, "/") {
		return "", false
	}
	return deviceID, true
}

// ParseTopic parses one concrete wire topic. Wildcards are rejected.
func ParseTopic(topic string) (Topic, error) {
	if !strings.HasPrefix(topic, topicRoot) {
		return Topic{}, ErrInvalidTopic
	}
	parts := strings.Split(strings.TrimPrefix(topic, topicRoot), "/")
	switch len(parts) {
	case 2:
		if !validID(parts[0]) {
			return Topic{}, ErrInvalidTopic
		}
		switch TopicKind(parts[1]) {
		case KindRequest, KindResult, KindEvent, KindPresence:
			return Topic{HostID: parts[0], Kind: TopicKind(parts[1])}, nil
		default:
			return Topic{}, ErrInvalidTopic
		}
	case 4:
		if !validID(parts[0]) || parts[1] != "d" || !validID(parts[2]) || (parts[3] != "event" && parts[3] != "ack") {
			return Topic{}, ErrInvalidTopic
		}
		kind := KindDeviceEvent
		if parts[3] == "ack" {
			kind = KindDeviceAck
		}
		return Topic{HostID: parts[0], DeviceID: parts[2], Kind: kind}, nil
	default:
		return Topic{}, ErrInvalidTopic
	}
}

// ClientIDForHost returns the fixed host client id.
func ClientIDForHost(hostID string) string { return "whalerein-host-" + hostID }

// ClientIDForDevice returns the fixed device client id.
func ClientIDForDevice(deviceID string) string { return "whalerein-device-" + deviceID }
