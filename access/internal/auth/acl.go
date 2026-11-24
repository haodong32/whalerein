package auth

import (
	"context"
	"fmt"
	"strings"
)

// ACLAction is the broker policy action projected to a connection.
type ACLAction string

// Supported ACL actions.
const (
	ActionPublish   ACLAction = "publish"
	ActionSubscribe ACLAction = "subscribe"
)

const (
	topicPrefix       = "whalerein/v1/h/"
	accessResult      = "whalerein/v1/h/+/result"
	accessEvent       = "whalerein/v1/h/+/event"
	accessDeviceQuery = "whalerein/v1/h/+/d/+/query"
	accessDeviceReady = "whalerein/v1/h/+/d/+/ready"
	accessDeviceWill  = "whalerein/v1/presence/device/+"
	deviceWillPrefix  = "whalerein/v1/presence/device/"
	sysConnected      = "$SYS/brokers/+/clients/+/connected"
	sysDisconnected   = "$SYS/brokers/+/clients/+/disconnected"
)

// AuthorizeACL evaluates the exact topic rules for the authenticated broker
// principal. Host and device filters may never contain wildcards. A denied
// request returns (false, nil); infrastructure failures return an error.
func (s *Service) AuthorizeACL(ctx context.Context, identity BrokerIdentity, action ACLAction, topic string) (bool, error) {
	action = ACLAction(strings.ToLower(string(action)))
	if action != ActionPublish && action != ActionSubscribe {
		return false, nil
	}
	switch identity.Kind {
	case KindAccess:
		return s.authorizeAccessACL(action, topic), nil
	case KindHost:
		return s.authorizeHostACL(identity, action, topic), nil
	case KindDevice:
		return s.authorizeDeviceACL(ctx, identity, action, topic)
	default:
		return false, nil
	}
}

func (s *Service) authorizeAccessACL(action ACLAction, topic string) bool {
	switch action {
	case ActionSubscribe:
		// Access needs results, control events, device queries, device
		// advertisements, device last wills and broker presence system topics.
		// Previews now travel directly from the Host, so Access no longer reads
		// h/{host}/live. There is no device ACK subscription.
		return topic == accessResult || topic == accessEvent ||
			topic == accessDeviceQuery ||
			topic == sysConnected || topic == sysDisconnected ||
			topic == accessDeviceWill || topic == accessDeviceReady
	case ActionPublish:
		parsed, ok := parseTopic(topic)
		if !ok {
			return false
		}
		return parsed.kind == "request" ||
			(parsed.kind == "device_event" && parsed.hostID != "") ||
			(parsed.kind == "device_notice" && parsed.hostID != "") ||
			(parsed.kind == "device_reply" && parsed.hostID != "") ||
			parsed.kind == "presence"
	}
	return false
}

func (s *Service) authorizeHostACL(identity BrokerIdentity, action ACLAction, topic string) bool {
	if strings.ContainsAny(topic, "+#") {
		return false
	}
	parsed, ok := parseTopic(topic)
	if !ok || parsed.hostID != identity.HostID {
		return false
	}
	switch action {
	case ActionSubscribe:
		return parsed.kind == "request" || parsed.kind == "presence"
	case ActionPublish:
		// A Host publishes results, control events and its authorized direct
		// device previews. The retired h/{host}/live relay is never allowed.
		return parsed.kind == "result" || parsed.kind == "event" ||
			(parsed.kind == "device_live" && parsed.hostID == identity.HostID)
	}
	return false
}

func (s *Service) authorizeDeviceACL(ctx context.Context, identity BrokerIdentity, action ACLAction, topic string) (bool, error) {
	if strings.ContainsAny(topic, "+#") {
		return false, nil
	}
	// A device may publish only its own non-retained last will. This does not
	// depend on a grant: the event cannot name another device or host.
	if action == ActionPublish {
		if deviceID, ok := parseDeviceWillTopic(topic); ok {
			return identity.DeviceID != "" && deviceID == identity.DeviceID, nil
		}
	}
	parsed, ok := parseTopic(topic)
	if !ok {
		return false, nil
	}
	allowed := (action == ActionSubscribe && (parsed.kind == "device_event" ||
		parsed.kind == "device_notice" || parsed.kind == "device_live" ||
		parsed.kind == "device_reply")) ||
		(action == ActionPublish && (parsed.kind == "device_ready" || parsed.kind == "device_query"))
	if !allowed {
		return false, nil
	}
	if parsed.deviceID != identity.DeviceID {
		return false, nil
	}
	active, err := s.admission.Authorize(ctx, parsed.hostID, identity.DeviceID)
	if err != nil {
		return false, fmt.Errorf("authorize device topic: %w", err)
	}
	return active, nil
}

type parsedTopic struct {
	hostID   string
	deviceID string
	kind     string
}

// parseTopic parses one of the complete-message wire topics:
//
//	whalerein/v1/h/{host_id}/request
//	whalerein/v1/h/{host_id}/result
//	whalerein/v1/h/{host_id}/event
//	whalerein/v1/h/{host_id}/d/{device_id}/event
//	whalerein/v1/h/{host_id}/d/{device_id}/notice
//	whalerein/v1/h/{host_id}/d/{device_id}/ready
//	whalerein/v1/h/{host_id}/d/{device_id}/query
//	whalerein/v1/h/{host_id}/d/{device_id}/reply
//	whalerein/v1/h/{host_id}/d/{device_id}/live
func parseTopic(topic string) (parsedTopic, bool) {
	if !strings.HasPrefix(topic, topicPrefix) {
		return parsedTopic{}, false
	}
	rest := strings.TrimPrefix(topic, topicPrefix)
	parts := strings.Split(rest, "/")
	switch len(parts) {
	case 2:
		hostID, leaf := parts[0], parts[1]
		if hostID == "" || strings.ContainsAny(hostID, "+#") {
			return parsedTopic{}, false
		}
		switch leaf {
		case "request", "result", "event", "presence":
			return parsedTopic{hostID: hostID, kind: leaf}, true
		default:
			return parsedTopic{}, false
		}
	case 4:
		hostID, marker, deviceID, leaf := parts[0], parts[1], parts[2], parts[3]
		if hostID == "" || deviceID == "" || strings.ContainsAny(hostID+deviceID, "+#") {
			return parsedTopic{}, false
		}
		if marker != "d" {
			return parsedTopic{}, false
		}
		switch leaf {
		case "event":
			return parsedTopic{hostID: hostID, deviceID: deviceID, kind: "device_event"}, true
		case "notice":
			return parsedTopic{hostID: hostID, deviceID: deviceID, kind: "device_notice"}, true
		case "ready":
			return parsedTopic{hostID: hostID, deviceID: deviceID, kind: "device_ready"}, true
		case "query":
			return parsedTopic{hostID: hostID, deviceID: deviceID, kind: "device_query"}, true
		case "reply":
			return parsedTopic{hostID: hostID, deviceID: deviceID, kind: "device_reply"}, true
		case "live":
			return parsedTopic{hostID: hostID, deviceID: deviceID, kind: "device_live"}, true
		default:
			return parsedTopic{}, false
		}
	default:
		return parsedTopic{}, false
	}
}

// parseDeviceWillTopic parses the device-scoped last-will topic:
//
//	whalerein/v1/presence/device/{device_id}
func parseDeviceWillTopic(topic string) (string, bool) {
	if !strings.HasPrefix(topic, deviceWillPrefix) {
		return "", false
	}
	deviceID := strings.TrimPrefix(topic, deviceWillPrefix)
	if deviceID == "" || strings.ContainsAny(deviceID, "/+#") {
		return "", false
	}
	return deviceID, true
}
