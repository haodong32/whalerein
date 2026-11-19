//go:build integration

package store_test

import (
	"bytes"
	"context"
	"crypto/sha256"
	"errors"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
	"github.com/whalerein/whalerein/access/internal/store/storetest"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

func boundWatch(t *testing.T, s *store.Store, ctx context.Context, requestID, host, device, session string, now time.Time) {
	t.Helper()
	if _, err := s.CreateRequest(ctx, store.Request{
		RequestID: requestID, HostID: host, DeviceID: device, DeviceSessionID: session,
		PayloadDigest: bytes.Repeat([]byte{2}, 32), Kind: whalereinv1.RequestKind_REQUEST_KIND_COMMAND,
		Status: whalereinv1.OnlineStatus_received, ProtocolVersion: 1, ExpiresAt: now.Add(time.Minute),
		Stamp: &store.GrantStamp{GrantGeneration: 1, RevocationGeneration: 1, DeviceRevocationGeneration: 1, ResultLimitBytes: 16 << 20},
	}); err != nil {
		t.Fatal(err)
	}
}

// A successful permission renewal keeps the active watch and scoped operations
// on the original Device grant and session identity. An interrupted renewal
// leaves the previous permission usable through its deadline and unusable
// afterwards.
func TestPermissionRenewalKeepsTheWatchAndInterruptionExpiresAtTheDeadline(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	// PostgreSQL timestamps keep microsecond precision; keep the expected
	// value at that precision so the round trip compares exactly.
	now := time.Now().UTC().Truncate(time.Microsecond)
	const workspace, host, device, revision, authority = "workspace", "host", "device", "1", "authority"
	enrollCloudHost(t, s, workspace, host, revision, "enroll", now)
	confirmCloudGrant(t, s, workspace, host, device, revision, "claim", now)

	digest := sha256.Sum256([]byte("permission credential"))
	permission := store.DeviceSession{
		ID: "session", WorkspaceID: workspace, RegionID: "region", DeviceID: device,
		KeyFingerprint: fixtureKeyPin(3), AuthorityRevision: authority,
		CredentialHash: digest[:], Revision: 1, ExpiresAt: now.Add(time.Minute),
	}
	if err := s.InstallDeviceSession(ctx, permission); err != nil {
		t.Fatal(err)
	}
	scoped := store.WithDeviceSessionAuthority(ctx, authority)
	boundWatch(t, s, scoped, "watch", host, device, permission.ID, now)
	if err := s.PublishDuringDeviceSession(scoped, "watch", func() error { return nil }); err != nil {
		t.Fatalf("baseline watch was denied: %v", err)
	}

	// Renewal extends the same session and keeps the original grant scope.
	renewed := permission
	renewed.Revision, renewed.ExpiresAt = 2, now.Add(2*time.Minute)
	if err := s.InstallDeviceSession(ctx, renewed); err != nil {
		t.Fatalf("renewal failed: %v", err)
	}
	stored, err := s.DeviceSessionByCredential(ctx, digest[:], now.Add(90*time.Second))
	if err != nil || stored.ID != permission.ID || stored.Revision != 2 || !stored.ExpiresAt.Equal(renewed.ExpiresAt) {
		t.Fatalf("renewed permission = %+v, err=%v", stored, err)
	}
	if stored.AuthorityRevision != authority || stored.WorkspaceID != workspace || stored.DeviceID != device {
		t.Fatal("renewal changed the original grant scope")
	}
	hosts, err := s.DeviceSessionHosts(ctx, stored)
	if err != nil || !hosts[host] {
		t.Fatalf("renewal lost the original grant: %v, %v", hosts, err)
	}
	if err := s.PublishDuringDeviceSession(scoped, "watch", func() error { return nil }); err != nil {
		t.Fatalf("watch did not survive renewal: %v", err)
	}

	// An interrupted renewal leaves the previous deadline in force. The watch
	// stays usable before it and is denied once the permission expires.
	interrupted := sha256.Sum256([]byte("interrupted credential"))
	stale := permission
	stale.ID, stale.CredentialHash = "interrupted-session", interrupted[:]
	if err := s.InstallDeviceSession(ctx, stale); err != nil {
		t.Fatal(err)
	}
	interruptedWatch := "interrupted-watch"
	boundWatch(t, s, scoped, interruptedWatch, host, device, stale.ID, now)
	if err := s.PublishDuringDeviceSession(scoped, interruptedWatch, func() error { return nil }); err != nil {
		t.Fatalf("stale permission denied before its deadline: %v", err)
	}
	if _, err := s.ActiveDeviceSession(ctx, stale.ID, stale.ExpiresAt.Add(-time.Millisecond)); err != nil {
		t.Fatalf("stale permission was not usable through its deadline: %v", err)
	}
	if _, err := s.ActiveDeviceSession(ctx, stale.ID, stale.ExpiresAt); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("stale permission remained usable after its deadline: %v", err)
	}
	// Model the deadline passing for the database-clock forwarding gate.
	if _, err := s.Pool().Exec(ctx, `UPDATE device_sessions SET expires_at=$2 WHERE session_id=$1`, stale.ID, now.Add(-time.Second)); err != nil {
		t.Fatal(err)
	}
	if err := s.PublishDuringDeviceSession(scoped, interruptedWatch, func() error {
		t.Fatal("expired permission reached the forwarding callback")
		return nil
	}); !errors.Is(err, store.ErrDeviceSessionEnded) {
		t.Fatalf("expired permission sustained the watch: %v", err)
	}
}
