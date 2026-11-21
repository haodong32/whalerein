package requests

import (
	"context"
	"errors"
	"reflect"
	"testing"

	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

// The topic is assumed authenticated by the broker. Conflicting supplied
// authority fields must not write; legacy results may omit optional fields.
func TestRejectedHostResultsCannotMutateAnotherAuthority(t *testing.T) {
	cases := []struct {
		name   string
		topic  string
		mutate func(*whalereinv1.HostResult)
	}{
		{"result-host", "h1", func(r *whalereinv1.HostResult) { r.HostId = "other-host" }},
		{"topic-host", "other-host", func(*whalereinv1.HostResult) {}},
		{"device", "h1", func(r *whalereinv1.HostResult) { r.DeviceId = "other-device" }},
		{"command", "h1", func(r *whalereinv1.HostResult) { r.CommandId = "other-command" }},
		{"digest", "h1", func(r *whalereinv1.HostResult) { r.PayloadDigest = []byte{9} }},
		{"channel", "h1", func(r *whalereinv1.HostResult) { r.ChannelId = "other-channel" }},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			f := newFixture(t)
			f.store.rows["r1"] = store.Request{RequestID: "r1", HostID: "h1", DeviceID: "d1", CommandID: "c1",
				ChannelID: "channel-1", PayloadDigest: []byte{1}, Status: whalereinv1.OnlineStatus_received}
			valid := &whalereinv1.HostResult{RequestId: "r1", DeviceId: "d1", CommandId: "c1",
				ChannelId: "channel-1", PayloadDigest: []byte{1}, Status: whalereinv1.OnlineStatus_succeeded, RelayVersion: 3}
			forged := proto.Clone(valid).(*whalereinv1.HostResult)
			tc.mutate(forged)
			if err := f.service.HandleHostResult(context.Background(), tc.topic, forged); err == nil {
				t.Fatal("conflicting result was accepted")
			}
			if len(f.store.applyCalls) != 0 {
				t.Fatal("rejected authority reached persistence")
			}
			if err := f.service.HandleHostResult(context.Background(), "h1", valid); err != nil {
				t.Fatalf("valid result after rejection = %v", err)
			}
			if len(f.store.applyCalls) != 1 {
				t.Fatal("valid result was not applied once")
			}
		})
	}
}

// This asserts only service-level errors and returned metadata, not HTTP or timing behavior.
func TestForeignDeviceAndMissingRequestReturnEmptyServiceResults(t *testing.T) {
	f := newFixture(t)
	f.store.rows["r1"] = store.Request{RequestID: "r1", DeviceID: "owner", Status: whalereinv1.OnlineStatus_succeeded}
	foreign, foreignErr := f.service.Get(context.Background(), "other", "r1")
	missing, missingErr := f.service.Get(context.Background(), "other", "missing")
	if !errors.Is(foreignErr, ErrNotFound) || !errors.Is(missingErr, ErrNotFound) {
		t.Fatalf("foreign error=%v missing error=%v", foreignErr, missingErr)
	}
	if !reflect.DeepEqual(foreign, store.Request{}) || !reflect.DeepEqual(missing, store.Request{}) {
		t.Fatal("request metadata leaked to foreign device")
	}
}
