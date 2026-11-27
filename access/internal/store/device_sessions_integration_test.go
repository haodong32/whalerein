//go:build integration

package store_test

import (
	"context"
	"crypto/sha256"
	"errors"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
	"github.com/whalerein/whalerein/access/internal/store/storetest"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// A newer permission renews only its own session. A terminal fence wins
// against reordering and does not end a subsequent login on the same Device.
func TestDeviceSessionPermissionRenewalAndFenceOrdering(t *testing.T) {
	s, _ := storetest.NewSQLite(t)
	ctx := context.Background()
	now := time.Now().UTC().Truncate(time.Millisecond)
	firstToken := sha256.Sum256([]byte("first test credential"))
	permission := store.DeviceSession{
		ID: "session-a", WorkspaceID: "workspace", RegionID: "region", DeviceID: "device",
		KeyFingerprint: "key", AuthorityRevision: "revision", CredentialHash: firstToken[:],
		Revision: 1, ExpiresAt: now.Add(time.Minute),
	}
	check := func(expectedRevision int64, expectedExpiry time.Time) {
		t.Helper()
		got, err := s.DeviceSessionByCredential(ctx, permission.CredentialHash, now)
		if err != nil || got.ID != permission.ID || got.Revision != expectedRevision || !got.ExpiresAt.Equal(expectedExpiry) {
			t.Fatalf("permission session=%q revision=%d expiry=%v, err=%v", got.ID, got.Revision, got.ExpiresAt, err)
		}
	}
	if err := s.InstallDeviceSession(ctx, permission); err != nil {
		t.Fatal(err)
	}
	check(1, permission.ExpiresAt)
	renewed := permission
	renewed.Revision = 2
	renewed.ExpiresAt = now.Add(2 * time.Minute)
	if err := s.InstallDeviceSession(ctx, renewed); err != nil {
		t.Fatal(err)
	}
	check(2, renewed.ExpiresAt)
	if err := s.InstallDeviceSession(ctx, permission); !errors.Is(err, store.ErrPairingFenced) {
		t.Fatalf("stale renewal: %v", err)
	}
	if _, err := s.DeviceSessionByCredential(ctx, permission.CredentialHash, renewed.ExpiresAt); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("expiry is exclusive: %v", err)
	}
	secondToken := sha256.Sum256([]byte("second test credential"))
	second := permission
	second.ID, second.CredentialHash = "session-b", secondToken[:]
	if err := s.InstallDeviceSession(ctx, second); err != nil {
		t.Fatal(err)
	}
	if err := s.RevokeDeviceSession(ctx, permission.ID, 3); err != nil {
		t.Fatal(err)
	}
	renewed.Revision = 4
	if err := s.InstallDeviceSession(ctx, renewed); !errors.Is(err, store.ErrPairingFenced) {
		t.Fatalf("delayed permission reopened revoked session: %v", err)
	}
	if _, err := s.DeviceSessionByCredential(ctx, permission.CredentialHash, now); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("revoked credential remained active: %v", err)
	}
	if got, err := s.DeviceSessionByCredential(ctx, second.CredentialHash, now); err != nil || got.ID != second.ID {
		t.Fatalf("revocation affected sibling current session: %q, %v", got.ID, err)
	}
	futureToken := sha256.Sum256([]byte("not yet installed fixture credential"))
	third := permission
	third.ID, third.CredentialHash = "session-c", futureToken[:]
	if err := s.RevokeDeviceSession(ctx, third.ID, 2); err != nil {
		t.Fatal(err)
	}
	if err := s.InstallDeviceSession(ctx, third); !errors.Is(err, store.ErrPairingFenced) {
		t.Fatalf("revocation-before-install lost its fence: %v", err)
	}
}

// Revocation blocks queued forwarding and retains the original request and
// command identity for exact cancellation after a restart.
func TestRevokedPermissionStopsForwardingAndPreservesExactCancellation(t *testing.T) {
	s, _ := storetest.NewSQLite(t)
	ctx := context.Background()
	now := time.Now().UTC()
	if _, err := s.EnsureHost(ctx, "session-host"); err != nil {
		t.Fatal(err)
	}
	digest := sha256.Sum256([]byte("test credential"))
	if err := s.CreatePairedDevice(ctx, "session-host", "device", "key", digest[:], now); err != nil {
		t.Fatal(err)
	}

	permission := store.DeviceSession{
		ID: "session", WorkspaceID: "workspace", RegionID: "region", DeviceID: "device",
		KeyFingerprint: "key", AuthorityRevision: "revision", CredentialHash: digest[:],
		Revision: 1, ExpiresAt: now.Add(time.Minute),
	}
	if err := s.InstallDeviceSession(ctx, permission); err != nil {
		t.Fatal(err)
	}
	request := store.Request{
		RequestID: "original-request", HostID: "session-host", DeviceID: "device",
		DeviceSessionID: permission.ID, CommandID: "original-command",
		PayloadDigest: digest[:], Kind: whalereinv1.RequestKind_REQUEST_KIND_COMMAND,
		Status: whalereinv1.OnlineStatus_received, ProtocolVersion: 1,
		ExpiresAt: now.Add(time.Minute),
		Stamp:     &store.GrantStamp{GrantGeneration: 1, RevocationGeneration: 1, DeviceRevocationGeneration: 1, ResultLimitBytes: 16 << 20},
	}
	if _, err := s.CreateRequest(ctx, request); err != nil {
		t.Fatal(err)
	}
	// A recovered node can see the old rows before the current permission
	// arrives. Neither that state nor renewal under the new authority may
	// revive work admitted under the previous authority.
	current := store.WithDeviceSessionAuthority(ctx, "next-revision")
	assertFenced := func() {
		t.Helper()
		err := s.PublishDuringDeviceSession(current, request.RequestID, func() error {
			t.Fatal("previous authority reached the forwarding callback")
			return nil
		})
		if !errors.Is(err, store.ErrDeviceSessionEnded) {
			t.Fatalf("serving authority fence: %v", err)
		}
	}
	assertFenced()
	next := permission
	next.AuthorityRevision, next.Revision = "next-revision", 2
	if err := s.InstallDeviceSession(current, next); err != nil {
		t.Fatal(err)
	}
	assertFenced()
	if ended, err := s.SessionCancellationReason(current, request.RequestID); err != nil || !ended {
		t.Fatalf("previous authority cancellation reason=%v err=%v", ended, err)
	}
	if err := s.RevokeDeviceSession(ctx, permission.ID, 3); err != nil {
		t.Fatal(err)
	}
	if err := s.PublishDuringDeviceSession(ctx, request.RequestID, func() error {
		t.Fatal("revoked command reached the forwarding callback")
		return nil
	}); !errors.Is(err, store.ErrDeviceSessionEnded) {
		t.Fatalf("forwarding fence: %v", err)
	}
	if expired, err := s.ExpireClientLeases(ctx, 10); err != nil || len(expired) != 1 ||
		expired[0].RequestID != request.RequestID || expired[0].CommandID != request.CommandID || !expired[0].CancelPending {
		t.Fatalf("expected original request's exact cancellation work, rows=%d err=%v", len(expired), err)
	}
	if ended, err := s.SessionCancellationReason(ctx, request.RequestID); err != nil || !ended {
		t.Fatalf("managed cancellation reason=%v err=%v", ended, err)
	}
}
