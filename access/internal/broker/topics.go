package broker

import (
	"errors"
	"strings"
)

// MQTT topics and filters from proto/v1/WIRE.md.
const (
	topicRoot = "whalerein/v1/h/"

	// ResultFilter receives HostResult messages from every host.
	ResultFilter = "whalerein/v1/h/+/result"
	// EventFilter receives event envelopes from every host.
	EventFilter = "whalerein/v1/h/+/event"
	// DeviceQueryFilter receives deadline-bound query and handshake requests
	// from devices.
	DeviceQueryFilter = "whalerein/v1/h/+/d/+/query"
	// DeviceReadyFilter receives Device receive advertisements. Access is the
	// only subscriber; a Device may publish only its own ready topic.
	DeviceReadyFilter = "whalerein/v1/h/+/d/+/ready"
	// SelfStatusReplyFilter receives Host replies for the bounded self-status
	// bootstrap exchange. Access is the only subscriber and a Device can never
	// publish or subscribe on this service subtree.
	SelfStatusReplyFilter = "whalerein/v1/h/+/svc/self-status/reply"
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
	KindDeviceQuery  TopicKind = "device_query"
	KindDeviceReply  TopicKind = "device_reply"
	KindDeviceLive   TopicKind = "device_live"
	KindPresence     TopicKind = "presence"

	// Service-only bootstrap lanes. These are not ordinary request topics: a
	// Device is denied on both, and only Access publishes the request while only
	// the addressed Host publishes the reply.
	KindServiceSelfStatusRequest TopicKind = "service_self_status_request"
	KindServiceSelfStatusReply   TopicKind = "service_self_status_reply"
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

// EventTopic is the host -> Access control event topic.
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

// DeviceLiveTopic is the Host -> device lossy preview topic. Access no longer
// republishes previews, but the topic remains part of the wire contract.
func DeviceLiveTopic(hostID, deviceID string) string {
	return topicRoot + hostID + "/d/" + deviceID + "/live"
}

// DeviceQueryTopic is the device -> Access query and handshake topic.
func DeviceQueryTopic(hostID, deviceID string) string {
	return topicRoot + hostID + "/d/" + deviceID + "/query"
}

// DeviceReplyTopic is the Access -> device query reply topic. It is the only
// authorized MQTT Response Topic for a query-lane request.
func DeviceReplyTopic(hostID, deviceID string) string {
	return topicRoot + hostID + "/d/" + deviceID + "/reply"
}

// PresenceTopic is the Access -> host device-presence topic.
func PresenceTopic(hostID string) string { return topicRoot + hostID + "/presence" }

// SelfStatusRequestTopic is the Access -> Host bounded self-status topic. It is
// the only topic on which Access forwards a status-only bootstrap frame.
func SelfStatusRequestTopic(hostID string) string {
	return topicRoot + hostID + "/svc/self-status/request"
}

// SelfStatusReplyTopic is the Host -> Access bounded self-status reply topic.
// A Device is denied on it by broker policy; only the addressed Host publishes.
func SelfStatusReplyTopic(hostID string) string {
	return topicRoot + hostID + "/svc/self-status/reply"
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
		if !validID(parts[0]) {
			return Topic{}, ErrInvalidTopic
		}
		// Service-only bootstrap subtree: h/{host}/svc/self-status/{request|reply}.
		// It is disjoint from the Device subtree so no Device identifier can
		// address it and no ordinary parser branch admits it by accident.
		if parts[1] == "svc" {
			if parts[2] != "self-status" {
				return Topic{}, ErrInvalidTopic
			}
			switch parts[3] {
			case "request":
				return Topic{HostID: parts[0], Kind: KindServiceSelfStatusRequest}, nil
			case "reply":
				return Topic{HostID: parts[0], Kind: KindServiceSelfStatusReply}, nil
			default:
				return Topic{}, ErrInvalidTopic
			}
		}
		if parts[1] != "d" || !validID(parts[2]) {
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
		case "query":
			kind = KindDeviceQuery
		case "reply":
			kind = KindDeviceReply
		case "live":
			kind = KindDeviceLive
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
