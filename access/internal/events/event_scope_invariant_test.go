package events

import (
	"bytes"
	"context"
	"errors"
	"testing"
)

// The handler uses supplied grant state; this does not exercise revocation or encryption.
func TestEventRoutingUsesExactHostDeviceGrant(t *testing.T) {
	h, grants, b := newHandler(t)
	ctx := context.Background()
	grants.active["h2|d1"] = true
	grants.active["h1|d2"] = true
	grants.active["h1|d1"] = false
	payload := envelope(t, "h1", "d1")
	if err := h.Handle(ctx, "h1", payload); err != nil {
		t.Fatal(err)
	}
	if b.calls != 0 {
		t.Fatal("another host or device grant authorized an inactive tuple")
	}
	grants.err = errors.New("grant lookup unavailable")
	if err := h.Handle(ctx, "h1", payload); err == nil {
		t.Fatal("grant failure was not reported")
	}
	if b.calls != 0 {
		t.Fatal("grant lookup failure published an event")
	}
	grants.err = nil
	valid := envelope(t, "h2", "d1")
	if err := h.Handle(ctx, "h2", valid); err != nil {
		t.Fatal(err)
	}
	if b.calls != 1 || b.hostID != "h2" || b.deviceID != "d1" || !bytes.Equal(b.payload, valid) {
		t.Fatal("an active tuple did not receive its original envelope")
	}
	other := envelope(t, "h1", "d2")
	if err := h.Handle(ctx, "h1", other); err != nil {
		t.Fatal(err)
	}
	if b.calls != 2 || b.hostID != "h1" || b.deviceID != "d2" || !bytes.Equal(b.payload, other) {
		t.Fatal("the other active device did not receive its original envelope")
	}
}
