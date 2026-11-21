package payload

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
)

func TestCheckReportsScopeAndLifecycleWithoutReading(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	object := objectFixture(now)
	object.State = store.PayloadReady
	metadata := &fakeStore{t: t,
		getFn: func(context.Context, string) (store.PayloadObject, error) {
			return object, nil
		},
	}
	service := newTestService(t, metadata, testConfig(t.TempDir()), now)
	ref := Ref{ObjectID: object.ObjectID, HostID: object.HostID, DeviceID: object.DeviceID, ChannelID: object.ChannelID, Purpose: object.Purpose}
	if _, err := service.Check(context.Background(), ref); err != nil {
		t.Fatalf("ready check: %v", err)
	}
	foreign := ref
	foreign.HostID = "other-host"
	if _, err := service.Check(context.Background(), foreign); !errors.Is(err, ErrForbidden) {
		t.Fatalf("foreign scope: %v", err)
	}
	object.State = store.PayloadReserved
	if _, err := service.Check(context.Background(), ref); !errors.Is(err, ErrNotReady) {
		t.Fatalf("reserved: %v", err)
	}
	object.State = store.PayloadDeleted
	if _, err := service.Check(context.Background(), ref); !errors.Is(err, ErrExpired) {
		t.Fatalf("deleted: %v", err)
	}
	object.State = store.PayloadReady
	object.ExpiresAt = now.Add(-time.Second)
	if _, err := service.Check(context.Background(), ref); !errors.Is(err, ErrExpired) {
		t.Fatalf("expired: %v", err)
	}
}

func TestCheckFailsClosedWhenDisabledOrMissing(t *testing.T) {
	service, err := New(Dependencies{Store: &fakeStore{t: t}, Config: Config{}})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := service.Check(context.Background(), Ref{ObjectID: "0123456789abcdef0123456789abcdef"}); !errors.Is(err, ErrDisabled) {
		t.Fatalf("disabled: %v", err)
	}
	now := time.Unix(1_700_000_000, 0).UTC()
	metadata := &fakeStore{t: t,
		getFn: func(context.Context, string) (store.PayloadObject, error) {
			return store.PayloadObject{}, store.ErrNotFound
		},
	}
	enabled := newTestService(t, metadata, testConfig(t.TempDir()), now)
	if _, err := enabled.Check(context.Background(), Ref{ObjectID: "not-hex"}); !errors.Is(err, ErrInvalid) {
		t.Fatalf("invalid ref: %v", err)
	}
	if _, err := enabled.Check(context.Background(), Ref{ObjectID: "0123456789abcdef0123456789abcdef"}); !errors.Is(err, ErrNotFound) {
		t.Fatalf("missing: %v", err)
	}
}

func TestReadyChecksDirectoryAndContext(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	service := newTestService(t, &fakeStore{t: t}, testConfig(t.TempDir()), now)
	if err := service.Ready(context.Background()); err != nil {
		t.Fatalf("ready: %v", err)
	}
	cancelled, cancel := context.WithCancel(context.Background())
	cancel()
	if err := service.Ready(cancelled); !errors.Is(err, context.Canceled) {
		t.Fatalf("cancelled ready: %v", err)
	}
	disabled, err := New(Dependencies{Store: &fakeStore{t: t}, Config: Config{}})
	if err != nil {
		t.Fatal(err)
	}
	if err := disabled.Ready(context.Background()); err != nil {
		t.Fatalf("disabled ready: %v", err)
	}
}
