package auth

import (
	"context"
	"testing"
)

// TestSelfStatusServiceTopicACLsMatchOnlyTheServicePrincipals pins the private
// service-only bootstrap lane: Access publishes the request and reads the reply,
// the addressed Host does the reverse, and a Device can never address it.
func TestSelfStatusServiceTopicACLsMatchOnlyTheServicePrincipals(t *testing.T) {
	service, fs, _, _ := newService(t)
	ctx := context.Background()
	fs.grants[grantKey("h1", "d1")] = true
	access := BrokerIdentity{Kind: KindAccess}
	host := BrokerIdentity{Kind: KindHost, HostID: "h1"}
	device := BrokerIdentity{Kind: KindDevice, DeviceID: "d1"}

	cases := []struct {
		name     string
		identity BrokerIdentity
		action   ACLAction
		topic    string
		allow    bool
	}{
		{"allows Access to subscribe to the service reply filter", access, ActionSubscribe, "whalerein/v1/h/+/svc/self-status/reply", true},
		{"denies Access a concrete service reply subscription", access, ActionSubscribe, "whalerein/v1/h/h1/svc/self-status/reply", false},
		{"allows Access to publish a service request", access, ActionPublish, "whalerein/v1/h/h1/svc/self-status/request", true},
		{"denies Access a service reply publish", access, ActionPublish, "whalerein/v1/h/h1/svc/self-status/reply", false},
		{"allows the addressed Host to subscribe to its service request", host, ActionSubscribe, "whalerein/v1/h/h1/svc/self-status/request", true},
		{"denies a Host another Host's service request", host, ActionSubscribe, "whalerein/v1/h/h2/svc/self-status/request", false},
		{"denies a Host wildcard service subscription", host, ActionSubscribe, "whalerein/v1/h/+/svc/self-status/request", false},
		{"allows the addressed Host to publish its service reply", host, ActionPublish, "whalerein/v1/h/h1/svc/self-status/reply", true},
		{"denies a Host another Host's service reply", host, ActionPublish, "whalerein/v1/h/h2/svc/self-status/reply", false},
		{"denies a Host a service request publish", host, ActionPublish, "whalerein/v1/h/h1/svc/self-status/request", false},
		{"denies a Device the service request publish", device, ActionPublish, "whalerein/v1/h/h1/svc/self-status/request", false},
		{"denies a Device the service reply subscribe", device, ActionSubscribe, "whalerein/v1/h/h1/svc/self-status/reply", false},
		{"denies a Device its own service subtree", device, ActionSubscribe, "whalerein/v1/h/h1/svc/self-status/request", false},
		{"denies a Device service wildcards", device, ActionPublish, "whalerein/v1/h/h1/svc/+/request", false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			allowed, err := service.AuthorizeACL(ctx, tc.identity, tc.action, tc.topic)
			if err != nil {
				t.Fatalf("err = %v", err)
			}
			if allowed != tc.allow {
				t.Fatalf("allowed = %v, want %v", allowed, tc.allow)
			}
		})
	}

	// A revoked Device remains denied even on the service subtree.
	fs.grants[grantKey("h1", "d1")] = false
	if allowed, _ := service.AuthorizeACL(ctx, device, ActionSubscribe, "whalerein/v1/h/h1/svc/self-status/reply"); allowed {
		t.Fatal("revoked device reached the service reply topic")
	}
}

// TestParseTopicRejectsEveryForeignShape keeps the topic grammar closed: the
// service branch is exact, and no malformed Host/Device segment can reach a
// policy decision.
func TestParseTopicRejectsEveryForeignShape(t *testing.T) {
	for _, topic := range []string{
		"whalerein/v1/h/", "whalerein/v1/h/h1", "whalerein/v1/h/+/request",
		"whalerein/v1/h/h1/other", "whalerein/v1/h/h1/svc/self-status",
		"whalerein/v1/h/h1/svc/other/request", "whalerein/v1/h/h1/svc/self-status/other",
		"whalerein/v1/h/h1/d//event", "whalerein/v1/h/h1/d/+/event",
		"whalerein/v1/h/h1/x/d1/event", "whalerein/v1/h/h1/d/d1/other",
		"whalerein/v1/h/+/svc/self-status/request", "other/v1/h/h1/request",
	} {
		if parsed, ok := parseTopic(topic); ok {
			t.Fatalf("parseTopic(%q) = %+v, want rejection", topic, parsed)
		}
	}
}
