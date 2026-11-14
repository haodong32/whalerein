package events

import (
	"bytes"
	"context"
	"testing"
)

// The handler uses supplied grant state; this does not exercise revocation or encryption.
func TestEventRoutingUsesExactHostDeviceGrant(t *testing.T) {
	h, grants, b := newHandler(t)
	ctx := context.Background()
	grants.grants["h2|d1"] = admissionState{allowed: true, known: true}
	grants.grants["h1|d2"] = admissionState{allowed: true, known: true}
	grants.grants["h1|d1"] = admissionState{allowed: false, known: true}
	payload := envelope(t, "h1", "d1")
	if err := h.Handle(ctx, "h1", payload); err != nil {
		t.Fatal(err)
	}
	if b.calls != 0 {
		t.Fatal("another host or device grant authorized an inactive tuple")
	}
	// An unknown grant fails closed and schedules a background load rather than
	// surfacing an error or forwarding the event.
	grants.grants["h1|d3"] = admissionState{allowed: false, known: false}
	other := envelope(t, "h1", "d3")
	if err := h.Handle(ctx, "h1", other); err != nil {
		t.Fatal(err)
	}
	if b.calls != 0 {
		t.Fatal("unknown grant published an event")
	}
	valid := envelope(t, "h2", "d1")
	if err := h.Handle(ctx, "h2", valid); err != nil {
		t.Fatal(err)
	}
	if b.calls != 1 || b.hostID != "h2" || b.deviceID != "d1" || !bytes.Equal(b.payload, valid) {
		t.Fatal("an active tuple did not receive its original envelope")
	}
	other = envelope(t, "h1", "d2")
	if err := h.Handle(ctx, "h1", other); err != nil {
		t.Fatal(err)
	}
	if b.calls != 2 || b.hostID != "h1" || b.deviceID != "d2" || !bytes.Equal(b.payload, other) {
		t.Fatal("the other active device did not receive its original envelope")
	}
}
