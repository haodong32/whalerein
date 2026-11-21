package auth

import (
	"context"
	"testing"
)

// ACL decisions use the supplied current grant state; no revocation transaction is exercised.
func TestACLUsesExactHostDeviceGrantForEventsNoticesAndReady(t *testing.T) {
	service, fs, _, _ := newService(t)
	ctx := context.Background()
	fs.grants[grantKey("h1", "d1")] = true
	fs.grants[grantKey("h2", "d1")] = true
	fs.grants[grantKey("h1", "d2")] = true
	first := BrokerIdentity{Kind: KindDevice, DeviceID: "d1"}
	second := BrokerIdentity{Kind: KindDevice, DeviceID: "d2"}
	check := func(identity BrokerIdentity, action ACLAction, topic string, expected bool) {
		t.Helper()
		allowed, err := service.AuthorizeACL(ctx, identity, action, topic)
		if err != nil || allowed != expected {
			t.Fatalf("action=%s allowed=%v err=%v", action, allowed, err)
		}
	}
	check(first, ActionSubscribe, "whalerein/v1/h/h1/d/d1/event", true)
	check(first, ActionSubscribe, "whalerein/v1/h/h1/d/d1/live", true)
	check(first, ActionPublish, "whalerein/v1/h/h1/d/d1/ready", true)
	fs.grants[grantKey("h1", "d1")] = false
	check(first, ActionSubscribe, "whalerein/v1/h/h1/d/d1/event", false)
	check(first, ActionSubscribe, "whalerein/v1/h/h1/d/d1/live", false)
	check(first, ActionPublish, "whalerein/v1/h/h1/d/d1/ready", false)
	check(first, ActionSubscribe, "whalerein/v1/h/h2/d/d1/event", true)
	check(first, ActionPublish, "whalerein/v1/h/h2/d/d1/ready", true)
	check(second, ActionSubscribe, "whalerein/v1/h/h1/d/d2/event", true)
	check(second, ActionSubscribe, "whalerein/v1/h/h1/d/d2/live", true)
	check(second, ActionPublish, "whalerein/v1/h/h1/d/d2/ready", true)
	check(first, ActionSubscribe, "whalerein/v1/h/h1/d/d2/event", false)
	check(first, ActionPublish, "whalerein/v1/h/h1/d/d2/ready", false)
	// Notices are Access-only publishes and Device subscriptions.
	check(first, ActionSubscribe, "whalerein/v1/h/h2/d/d1/notice", true)
	check(first, ActionPublish, "whalerein/v1/h/h2/d/d1/notice", false)
	// The removed acknowledgement topic is denied in both directions.
	check(first, ActionPublish, "whalerein/v1/h/h1/d/d1/ack", false)
	check(first, ActionSubscribe, "whalerein/v1/h/h1/d/d1/ack", false)
}
