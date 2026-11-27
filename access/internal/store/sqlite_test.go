package store_test

import (
	"context"
	"path/filepath"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

func openSQLite(t *testing.T) (*store.Store, context.Context) {
	t.Helper()
	ctx := context.Background()
	path := filepath.Join(t.TempDir(), "access.db")
	data, err := store.Open(ctx, path)
	if err != nil {
		t.Fatalf("open sqlite: %v", err)
	}
	t.Cleanup(data.Close)
	if err := data.Migrate(ctx); err != nil {
		t.Fatalf("migrate sqlite: %v", err)
	}
	return data, ctx
}

func TestSQLiteFreshInstallAndIdempotentMigration(t *testing.T) {
	data, ctx := openSQLite(t)
	if err := data.Migrate(ctx); err != nil {
		t.Fatalf("second migrate: %v", err)
	}
	if _, err := data.EnsureHost(ctx, "host"); err != nil {
		t.Fatalf("ensure host: %v", err)
	}
	if _, err := data.EnsureHost(ctx, "host"); err != nil {
		t.Fatalf("ensure host twice: %v", err)
	}
	host, err := data.GetHost(ctx, "host")
	if err != nil {
		t.Fatalf("get host: %v", err)
	}
	if host.HostID != "host" || host.CreatedAt.IsZero() {
		t.Fatalf("unexpected host: %#v", host)
	}
}

func TestSQLiteRequestRoundTripPreservesTimestampsAndBytes(t *testing.T) {
	data, ctx := openSQLite(t)
	now := time.Now().UTC().Truncate(time.Millisecond)
	if err := data.CreatePairedDevice(ctx, "host", "device", "fingerprint", []byte("auth"), now); err != nil {
		t.Fatal(err)
	}
	digest := make([]byte, 32)
	for i := range digest {
		digest[i] = byte(i)
	}
	request := store.Request{
		RequestID: "request-1", HostID: "host", DeviceID: "device",
		CommandID: "command-1", PayloadDigest: digest, Kind: 3,
		ProtocolVersion: 1, RelayVersion: 5, Status: whalereinv1.OnlineStatus_received,
		RequestBody: []byte{1, 2, 3, 0, 255},
		ExpiresAt:   now.Add(time.Hour), TraceID: "trace", CreatedAt: now, UpdatedAt: now,
		Stamp: &store.GrantStamp{ResultLimitBytes: 1 << 20},
	}
	if _, err := data.CreateRequest(ctx, request); err != nil {
		t.Fatalf("create request: %v", err)
	}
	loaded, err := data.GetRequest(ctx, "request-1")
	if err != nil {
		t.Fatalf("get request: %v", err)
	}
	if string(loaded.RequestBody) != string(request.RequestBody) {
		t.Fatalf("body changed: %v", loaded.RequestBody)
	}
	if !loaded.ExpiresAt.Equal(request.ExpiresAt) {
		t.Fatalf("expiry = %s, want %s", loaded.ExpiresAt, request.ExpiresAt)
	}
	if !loaded.CreatedAt.Equal(now) {
		t.Fatalf("created = %s, want %s", loaded.CreatedAt, now)
	}
}

func TestSQLiteRetainsUniqueCommandIdentity(t *testing.T) {
	data, ctx := openSQLite(t)
	now := time.Now().UTC()
	if err := data.CreatePairedDevice(ctx, "host", "device", "fingerprint", []byte("auth2"), now); err != nil {
		t.Fatal(err)
	}
	request := store.Request{
		RequestID: "request-1", HostID: "host", DeviceID: "device",
		CommandID: "same-command", Kind: 3, ProtocolVersion: 1, RelayVersion: 5,
		Status: whalereinv1.OnlineStatus_received, ExpiresAt: now.Add(time.Hour),
		CreatedAt: now, UpdatedAt: now,
		Stamp: &store.GrantStamp{ResultLimitBytes: 1 << 20},
	}
	if _, err := data.CreateRequest(ctx, request); err != nil {
		t.Fatalf("first create: %v", err)
	}
	duplicate := request
	duplicate.RequestID = "request-2"
	outcome, err := data.CreateRequest(ctx, duplicate)
	if err != nil {
		t.Fatalf("duplicate command identity: %v", err)
	}
	if outcome.Created || outcome.Request.RequestID != "request-1" {
		t.Fatalf("duplicate command identity created a second row: %#v", outcome)
	}
}
