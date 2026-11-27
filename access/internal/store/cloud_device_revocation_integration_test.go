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
	"google.golang.org/protobuf/proto"
)

// enrollCloudHost creates one ready external enrollment for a Host.
func enrollCloudHost(t *testing.T, s *store.Store, workspace, hostID, revision, operationID string, now time.Time) {
	t.Helper()
	ctx := context.Background()
	operation := store.ExternalOperation{
		OperationID: operationID, AuthorizationID: "auth-" + operationID,
		Purpose: "host_enrollment", WorkspaceID: workspace, RegionID: "region",
		HostID: hostID, PayloadDigest: []byte{1}, EnrollmentRevision: revision,
		State: "operation_committed",
	}
	_, _, _, err := s.EnrollHost(ctx, operation, store.ExternalEnrollment{
		HostID: hostID, WorkspaceID: workspace, EnrollmentRevision: revision,
		State: "enrollment_pending", HostSigningPublicKey: bytes.Repeat([]byte{3}, 32),
		HostNoisePublicKey: bytes.Repeat([]byte{1}, 32),
	})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := s.EnsureHost(ctx, hostID); err != nil {
		t.Fatal(err)
	}
	if _, advanced, err := s.MarkEnrollmentReady(ctx, hostID, now); err != nil || !advanced {
		t.Fatalf("enrollment %s did not become ready: %v", hostID, err)
	}
}

// confirmCloudGrant completes one Cloud pairing so the grant is authoritative
// for the Host client list.
func confirmCloudGrant(t *testing.T, s *store.Store, workspace, hostID, deviceID, revision, claimID string, now time.Time) {
	t.Helper()
	ctx := context.Background()
	digest := sha256.Sum256([]byte(claimID))
	hash := append([]byte{store.CloudPairingHashTag}, digest[:]...)
	if err := s.CreatePairingToken(ctx, hash, hostID, now.Add(time.Hour), now); err != nil {
		t.Fatal(err)
	}
	op, notice := fixturePairingOperation(t, s, hash, hostID, deviceID, claimID, now, nil)
	op.CloudWorkspaceID, op.CloudEnrollmentRevision = workspace, revision
	op.CloudAuthorizationExpiry = now.Add(time.Hour)
	committed, err := s.CommitPairing(ctx, hash, hostID, deviceID, fixtureKeyPin(3), bytes.Repeat([]byte{8}, 32), now, notice, &op)
	if err != nil || !committed {
		t.Fatalf("grant commit for %s/%s failed: %v", hostID, deviceID, err)
	}
	body, err := proto.Marshal(&whalereinv1.PairingClaimResult{Authorized: true, DeviceId: deviceID})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := s.ApplyHostResult(ctx, store.HostResultUpdate{
		RequestID: notice.RequestId, HostID: hostID, DeviceID: deviceID,
		Status: whalereinv1.OnlineStatus_succeeded, ResultBody: body, At: now,
	}); err != nil {
		t.Fatal(err)
	}
	if confirmed, err := s.ConfirmPairingHost(ctx, op.ClaimID, hostID, deviceID, notice.RequestId, now); err != nil || !confirmed {
		t.Fatalf("grant confirmation for %s/%s failed: %v", hostID, deviceID, err)
	}
}

// A pending revocation still consumes the Device slot, while enforcement
// releases it. Revocation is Host-scoped: the sibling Host keeps its grant and
// its confirmed client row, and the exact-scoped forwarding watch is denied
// before its ordinary deadline.
func TestCloudDeviceGrantRevocationIsHostScopedAndReleasesTheSlotAfterEnforcement(t *testing.T) {
	s, _ := storetest.NewSQLite(t)
	ctx := context.Background()
	now := time.Now().UTC()
	const workspace, device = "workspace", "device"
	enrollCloudHost(t, s, workspace, "host-a", "1", "enroll-a", now)
	enrollCloudHost(t, s, workspace, "host-b", "1", "enroll-b", now)
	confirmCloudGrant(t, s, workspace, "host-a", device, "1", "claim-a", now)
	confirmCloudGrant(t, s, workspace, "host-b", device, "1", "claim-b", now)

	for _, host := range []string{"host-a", "host-b"} {
		ids, err := s.HostClientIDs(ctx, workspace, host, "1")
		if err != nil || len(ids) != 1 || ids[0] != device {
			t.Fatalf("%s client list = %v, err=%v", host, ids, err)
		}
	}
	if count, err := s.CountActiveGrants(ctx, device); err != nil || count != 2 {
		t.Fatalf("initial Device slot usage = %d, err=%v", count, err)
	}
	if _, err := s.ActiveDeviceByAuthHash(ctx, bytes.Repeat([]byte{8}, 32)); err != nil {
		t.Fatalf("paired Device credential was not usable: %v", err)
	}

	// A watch bound to the original permission, with a normal deadline still in
	// the future, is denied only for the revoked Host scope.
	permission := store.DeviceSession{
		ID: "session", WorkspaceID: workspace, RegionID: "region", DeviceID: device,
		KeyFingerprint: fixtureKeyPin(3), AuthorityRevision: "authority",
		CredentialHash: bytes.Repeat([]byte{9}, 32), Revision: 1, ExpiresAt: now.Add(time.Minute),
	}
	if err := s.InstallDeviceSession(ctx, permission); err != nil {
		t.Fatal(err)
	}
	scoped := store.WithDeviceSessionAuthority(ctx, "authority")
	watch := func(host string) string {
		t.Helper()
		requestID := "watch-" + host
		if _, err := s.CreateRequest(scoped, store.Request{
			RequestID: requestID, HostID: host, DeviceID: device, DeviceSessionID: permission.ID,
			PayloadDigest: bytes.Repeat([]byte{2}, 32), Kind: whalereinv1.RequestKind_REQUEST_KIND_COMMAND,
			Stamp:  &store.GrantStamp{GrantGeneration: 1, RevocationGeneration: 1, DeviceRevocationGeneration: 1, ResultLimitBytes: 16 << 20},
			Status: whalereinv1.OnlineStatus_received, ProtocolVersion: 1, ExpiresAt: now.Add(time.Minute),
		}); err != nil {
			t.Fatal(err)
		}
		return requestID
	}
	watchA, watchB := watch("host-a"), watch("host-b")
	publish := func(requestID string) error {
		return s.PublishDuringDeviceSession(scoped, requestID, func() error { return nil })
	}
	for _, requestID := range []string{watchA, watchB} {
		if err := publish(requestID); err != nil {
			t.Fatalf("baseline watch %s was not admitted: %v", requestID, err)
		}
	}

	frozen, err := s.BeginCloudDeviceRevocation(ctx, workspace, "host-a", device, now)
	if err != nil || !frozen.Exists {
		t.Fatalf("begin revocation = %+v, err=%v", frozen, err)
	}
	if state, err := s.CloudDeviceGrantStatus(ctx, workspace, "host-a", device); err != nil || state != "pending" {
		t.Fatalf("pending state = %q, err=%v", state, err)
	}
	if state, err := s.CloudDeviceGrantStatus(ctx, workspace, "host-b", device); err != nil || state != "ready" {
		t.Fatalf("sibling state = %q, err=%v", state, err)
	}
	// The durable fence already removes the target row, but the pending
	// revocation still consumes its Device slot.
	if ids, err := s.HostClientIDs(ctx, workspace, "host-a", "1"); err != nil || len(ids) != 0 {
		t.Fatalf("pending revocation client list = %v, err=%v", ids, err)
	}
	if ids, err := s.HostClientIDs(ctx, workspace, "host-b", "1"); err != nil || len(ids) != 1 || ids[0] != device {
		t.Fatalf("sibling client list changed while pending = %v, err=%v", ids, err)
	}
	if count, err := s.CountActiveGrants(ctx, device); err != nil || count != 2 {
		t.Fatalf("pending revocation released the Device slot: %d, %v", count, err)
	}
	if err := publish(watchA); !errors.Is(err, store.ErrDeviceSessionEnded) {
		t.Fatalf("revoked Host watch was not denied: %v", err)
	}
	if err := publish(watchB); err != nil {
		t.Fatalf("sibling Host watch was denied: %v", err)
	}

	// Regional enforcement is the durable grant revocation; the acknowledgement
	// marker alone does not release the slot.
	if err := s.RevokeGrant(ctx, "host-a", device, now); err != nil {
		t.Fatal(err)
	}
	if err := s.CompleteCloudDeviceRevocation(ctx, "host-a", device, now); err != nil {
		t.Fatal(err)
	}
	if state, err := s.CloudDeviceGrantStatus(ctx, workspace, "host-a", device); err != nil || state != "revoked" {
		t.Fatalf("enforced state = %q, err=%v", state, err)
	}
	if active, err := s.HasActiveGrant(ctx, "host-a", device); err != nil || active {
		t.Fatalf("enforced grant still active: %v, %v", active, err)
	}
	if count, err := s.CountActiveGrants(ctx, device); err != nil || count != 1 {
		t.Fatalf("enforcement did not release exactly one slot: %d, %v", count, err)
	}
	if ids, err := s.HostClientIDs(ctx, workspace, "host-b", "1"); err != nil || len(ids) != 1 || ids[0] != device {
		t.Fatalf("enforcement removed the sibling Host row: %v, %v", ids, err)
	}

	// The last grant releases the final slot and the Device credential can no
	// longer reconnect.
	frozen, err = s.BeginCloudDeviceRevocation(ctx, workspace, "host-b", device, now)
	if err != nil || !frozen.Exists {
		t.Fatalf("begin sibling revocation = %+v, err=%v", frozen, err)
	}
	if err := s.RevokeGrant(ctx, "host-b", device, now); err != nil {
		t.Fatal(err)
	}
	if err := s.CompleteCloudDeviceRevocation(ctx, "host-b", device, now); err != nil {
		t.Fatal(err)
	}
	if count, err := s.CountActiveGrants(ctx, device); err != nil || count != 0 {
		t.Fatalf("last-grant revocation left slot usage %d, err=%v", count, err)
	}
	if _, err := s.ActiveDeviceByAuthHash(ctx, bytes.Repeat([]byte{8}, 32)); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("revoked Device credential still reconnected: %v", err)
	}
	if err := publish(watchB); !errors.Is(err, store.ErrDeviceSessionEnded) {
		t.Fatalf("last revoked Host watch was not denied: %v", err)
	}
}
