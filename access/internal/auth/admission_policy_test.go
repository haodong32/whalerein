package auth

import (
	"context"
	"errors"
	"reflect"
	"testing"

	"github.com/whalerein/whalerein/access/internal/policy"
	"github.com/whalerein/whalerein/access/internal/store"
)

func TestRevokeDeviceFencesAdmissionBeforeDurableWriteAndLeavesFenceOnFailure(t *testing.T) {
	service, fs, admission, _, _, enforcer := newServiceWithFakes(t)
	var events []string
	fs.events = &events
	admission.events = &events
	ctx := context.Background()
	fs.devices["d1"] = store.Device{DeviceID: "d1"}
	fs.hosts["h1"] = store.Host{HostID: "h1"}
	fs.grants[grantKey("h1", "d1")] = true

	if err := service.RevokeDevice(ctx, "d1"); err != nil {
		t.Fatal(err)
	}
	wantPrefix := []string{"admission.deny_device", "store.revoke_device", "admission.deny", "admission.complete_device_revoke"}
	if len(events) < len(wantPrefix) || !reflect.DeepEqual(events[:len(wantPrefix)], wantPrefix) {
		t.Fatalf("admission was not fenced before the durable write: %v", events)
	}
	if len(enforcer.calls) != 1 || enforcer.calls[0] != "device:d1" {
		t.Fatalf("enforcer calls = %v", enforcer.calls)
	}

	// A failed durable write must leave the provisional device fence in place.
	fs.devices["d2"] = store.Device{DeviceID: "d2"}
	fs.grants[grantKey("h1", "d2")] = true
	fs.revokeErr = errors.New("db down")
	events = nil
	if err := service.RevokeDevice(ctx, "d2"); err == nil {
		t.Fatal("expected durable write failure")
	}
	wantEvents := []string{"admission.deny_device", "store.revoke_device"}
	if !reflect.DeepEqual(events, wantEvents) {
		t.Fatalf("failed write events = %v", events)
	}
	if !admission.deviceDenied["d2"] {
		t.Fatal("device fence was lifted after a failed durable write")
	}
	if len(enforcer.calls) != 1 {
		t.Fatalf("enforcer ran despite the failed write: %v", enforcer.calls)
	}
}

func TestRevokeHostGrantFencesAdmissionBeforeWriteAndCallsEnforcerWithGrantScope(t *testing.T) {
	service, fs, admission, _, _, enforcer := newServiceWithFakes(t)
	var events []string
	fs.events = &events
	admission.events = &events
	ctx := context.Background()
	fs.hosts["h1"] = store.Host{HostID: "h1"}
	fs.grants[grantKey("h1", "d1")] = true

	if err := service.RevokeHostGrant(ctx, "h1", "d1"); err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(events, []string{"admission.deny", "store.revoke_grant", "admission.finish_grant_revoke"}) {
		t.Fatalf("events = %v", events)
	}
	if len(enforcer.calls) != 1 || enforcer.calls[0] != "grant:h1:d1" {
		t.Fatalf("enforcer calls = %v", enforcer.calls)
	}

	// A failed durable write keeps the admission fence so a concurrent dispatch
	// cannot use the grant being revoked.
	fs.grants[grantKey("h2", "d1")] = true
	service.store = failedGrantRevocation{fs}
	if err := service.RevokeHostGrant(ctx, "h2", "d1"); err == nil {
		t.Fatal("expected durable write failure")
	}
	if !admission.denied[grantKey("h2", "d1")] {
		t.Fatal("admission fence was lifted after a failed durable write")
	}
	if len(enforcer.calls) != 1 {
		t.Fatalf("enforcer ran despite the failed write: %v", enforcer.calls)
	}
}

func TestRevokeHostGrantPropagatesEnforcerFailureAfterDurableRevocation(t *testing.T) {
	service, fs, _, _, _, enforcer := newServiceWithFakes(t)
	ctx := context.Background()
	fs.hosts["h1"] = store.Host{HostID: "h1"}
	fs.grants[grantKey("h1", "d1")] = true
	enforcer.err = errors.New("broker policy unavailable")
	if err := service.RevokeHostGrant(ctx, "h1", "d1"); err == nil {
		t.Fatal("expected enforcer error")
	}
	if fs.grants[grantKey("h1", "d1")] {
		t.Fatal("durable revocation was rolled back by enforcer failure")
	}
}

func TestBrokerPolicyBuildsDeviceGrantsAndHostPolicyWithoutAStoreRead(t *testing.T) {
	service, fs, _, _, _, _ := newServiceWithFakes(t)
	ctx := context.Background()
	fs.grants[grantKey("h1", "d1")] = true
	fs.grants[grantKey("h2", "d1")] = false
	fs.grants[grantKey("h3", "d2")] = true

	device, err := service.BrokerPolicy(ctx, BrokerIdentity{Kind: KindDevice, DeviceID: "d1"})
	if err != nil {
		t.Fatal(err)
	}
	if device.Kind != policy.KindDevice || device.DeviceID != "d1" || device.Will != "whalerein/v1/presence/device/d1" {
		t.Fatalf("device policy = %+v", device)
	}
	if len(device.Entries) != 6 { // five topic entries for h1 plus the will.
		t.Fatalf("device entries = %+v", device.Entries)
	}
	for _, entry := range device.Entries {
		if entry.HostID != "" && entry.HostID != "h1" {
			t.Fatalf("revoked or foreign grant entered the policy: %+v", entry)
		}
	}
	if fs.listCalls != 1 {
		t.Fatalf("device policy store calls = %d", fs.listCalls)
	}

	host, err := service.BrokerPolicy(ctx, BrokerIdentity{Kind: KindHost, HostID: "h1"})
	if err != nil {
		t.Fatal(err)
	}
	if host.Kind != policy.KindHost || host.HostID != "h1" || len(host.Entries) != 6 {
		t.Fatalf("host policy = %+v", host)
	}
	if fs.listCalls != 1 {
		t.Fatalf("host policy performed a store read: %d calls", fs.listCalls)
	}

	fs.listGrantErr = errors.New("db down")
	if _, err := service.BrokerPolicy(ctx, BrokerIdentity{Kind: KindDevice, DeviceID: "d1"}); err == nil {
		t.Fatal("expected grant listing failure")
	}
}
