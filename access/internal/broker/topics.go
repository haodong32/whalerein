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
	EventFilter = "whalerein/v1/h/+/event"
	// DeviceReadyFilter receives Device receive advertisements. Access is the
	// only subscriber; a Device may publish only its own ready topic.
	DeviceReadyFilter = "whalerein/v1/h/+/d/+/ready"
	// DeviceWillFilter receives non-retained MQTT last wills published by the
	// broker for disconnected device connections.
	DeviceWillFilter = presenceRoot + "+"
)

// TopicKind is one of the complete-message wire topic shapes.
type TopicKind string

// Wire topic kinds. There is no device acknowledgement topic: the relay epoch
// carries one complete message per PUBLISH and no application receipts.
const (
	KindRequest      TopicKind = "request"
	KindResult       TopicKind = "result"
	KindEvent        TopicKind = "event"
	KindDeviceEvent  TopicKind = "device_event"
	KindDeviceNotice TopicKind = "device_notice"
	KindDeviceReady  TopicKind = "device_ready"
	KindPresence     TopicKind = "presence"
)

// ErrInvalidTopic is returned for topics outside the wire templates.
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

// DeviceNoticeTopic is the Access -> device result-notice topic. Only Access
// may publish it; a Device may subscribe to its own topic.
func DeviceNoticeTopic(hostID, deviceID string) string {
	return topicRoot + hostID + "/d/" + deviceID + "/notice"
}

// DeviceReadyTopic is the device -> Access receive-advertisement topic.
func DeviceReadyTopic(hostID, deviceID string) string {
	return topicRoot + hostID + "/d/" + deviceID + "/ready"
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
		if !validID(parts[0]) || parts[1] != "d" || !validID(parts[2]) {
			return Topic{}, ErrInvalidTopic
		}
		var kind TopicKind
		switch parts[3] {
		case "event":
			kind = KindDeviceEvent
		case "notice":
			kind = KindDeviceNotice
		case "ready":
			kind = KindDeviceReady
		default:
			return Topic{}, ErrInvalidTopic
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
