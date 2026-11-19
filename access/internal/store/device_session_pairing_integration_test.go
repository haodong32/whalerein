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

// boundCloudOperation saves one unfinished Cloud pairing intent bound to the
// installing App session.
func boundCloudOperation(t *testing.T, s *store.Store, hash []byte, workspace, hostID, deviceID, revision, claimID, sessionID, authority string, now time.Time) (store.PairingOperation, *whalereinv1.HostRequest) {
	t.Helper()
	ctx := store.WithDeviceSessionAuthority(context.Background(), authority)
	deviceGeneration, err := s.DeviceGeneration(ctx, deviceID)
	if err != nil {
		t.Fatal(err)
	}
	hostGeneration, err := s.HostGeneration(ctx, hostID, deviceID)
	if err != nil {
		t.Fatal(err)
	}
	publicKey := bytes.Repeat([]byte{3}, 32)
	op := store.PairingOperation{
		ClaimID: claimID, TokenHash: hash, HostID: hostID, DeviceID: deviceID,
		HelloHash: bytes.Repeat([]byte{1}, 32), HelloResponse: []byte("fixture"),
		ExpiresAt: now.Add(time.Hour), DeviceGeneration: deviceGeneration, HostGeneration: hostGeneration,
		FinishHash:       bytes.Repeat([]byte{2}, 32),
		CloudSessionID:   sessionID,
		CloudWorkspaceID: workspace, CloudEnrollmentRevision: revision,
		CloudAuthorizationExpiry: now.Add(time.Hour),
	}
	if err := s.SavePairingOperation(ctx, op); err != nil {
		t.Fatal(err)
	}
	op.FinishResponse, err = proto.Marshal(&whalereinv1.PairingClaimResponse{
		ClaimId: claimID, HostId: hostID, DeviceId: deviceID,
		Phase: whalereinv1.PairingPhase_PAIRING_PHASE_FINISH, DevicePublicKey: publicKey, ProtocolVersion: 1,
	})
	if err != nil {
		t.Fatal(err)
	}
	notice := &whalereinv1.HostRequest{
		RequestId: "fixture-" + claimID, HostId: hostID, DeviceId: deviceID,
		Kind: whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE, ProtocolVersion: 1,
		ExpiresAtUnixMs: op.ExpiresAt.UnixMilli(),
	}
	notice.Body, err = proto.Marshal(&whalereinv1.PairingClaim{
		ClaimId: claimID, DeviceId: deviceID, DevicePublicKey: publicKey,
		Phase: whalereinv1.PairingPhase_PAIRING_PHASE_COMMIT, ExpiresAtUnixMs: notice.ExpiresAtUnixMs,
	})
	if err != nil {
		t.Fatal(err)
	}
	return op, notice
}

// Changing the App session while a first pairing is unresolved refuses the old
// permit and its unfinished intent, while a durable grant that Access already
// confirmed remains usable from the new session without another pairing.
func TestCloudPairingRefusesASupersededAppSessionAndReusesAConfirmedGrant(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	now := time.Now().UTC()
	const workspace, host, device, revision, authority = "workspace", "host", "device", "1", "authority"
	enrollCloudHost(t, s, workspace, host, revision, "enroll", now)
	confirmCloudGrant(t, s, workspace, host, device, revision, "claim-durable", now)

	digestA := sha256.Sum256([]byte("app session A"))
	sessionA := store.DeviceSession{
		ID: "session-a", WorkspaceID: workspace, RegionID: "region", DeviceID: device,
		KeyFingerprint: fixtureKeyPin(3), AuthorityRevision: authority,
		CredentialHash: digestA[:], Revision: 1, ExpiresAt: now.Add(time.Minute),
	}
	if err := s.InstallDeviceSession(ctx, sessionA); err != nil {
		t.Fatal(err)
	}
	hash := append([]byte{store.CloudPairingHashTag}, bytes.Repeat([]byte{5}, 32)...)
	if err := s.CreatePairingToken(ctx, hash, host, now.Add(time.Hour), now); err != nil {
		t.Fatal(err)
	}
	op, notice := boundCloudOperation(t, s, hash, workspace, host, device, revision, "claim-unresolved", sessionA.ID, authority, now)

	// The App session changes before the unfinished intent commits.
	if err := s.RevokeDeviceSession(ctx, sessionA.ID, 2); err != nil {
		t.Fatal(err)
	}
	if _, err := s.DeviceSessionByCredential(ctx, sessionA.CredentialHash, now); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("revoked App session permit remained valid: %v", err)
	}
	scoped := store.WithDeviceSessionAuthority(ctx, authority)
	if _, err := s.CommitPairing(scoped, hash, host, device, fixtureKeyPin(3), bytes.Repeat([]byte{8}, 32), now, notice, &op); !errors.Is(err, store.ErrPairingFenced) {
		t.Fatalf("previous App session unfinished intent committed: %v", err)
	}
	// A renewal delivered after the revocation cannot reinstall the permit.
	renewedA := sessionA
	renewedA.Revision, renewedA.ExpiresAt = 2, now.Add(2*time.Minute)
	if err := s.InstallDeviceSession(ctx, renewedA); !errors.Is(err, store.ErrPairingFenced) {
		t.Fatalf("revocation between challenge and permit install was lost: %v", err)
	}

	// A valid new App session reuses the grant that Access already confirmed.
	digestB := sha256.Sum256([]byte("app session B"))
	sessionB := store.DeviceSession{
		ID: "session-b", WorkspaceID: workspace, RegionID: "region", DeviceID: device,
		KeyFingerprint: fixtureKeyPin(3), AuthorityRevision: authority,
		CredentialHash: digestB[:], Revision: 1, ExpiresAt: now.Add(time.Minute),
	}
	if err := s.InstallDeviceSession(ctx, sessionB); err != nil {
		t.Fatal(err)
	}
	hosts, err := s.DeviceSessionHosts(ctx, sessionB)
	if err != nil || !hosts[host] {
		t.Fatalf("new App session could not reuse the confirmed grant: %v, %v", hosts, err)
	}
	scoped = store.WithDeviceSessionAuthority(ctx, authority)
	if _, err := s.CreateRequest(scoped, store.Request{
		RequestID: "session-b-watch", HostID: host, DeviceID: device, DeviceSessionID: sessionB.ID,
		PayloadDigest: bytes.Repeat([]byte{2}, 32), Kind: whalereinv1.RequestKind_REQUEST_KIND_COMMAND,
		Status: whalereinv1.OnlineStatus_received, ProtocolVersion: 1, ExpiresAt: now.Add(time.Minute),
	}); err != nil {
		t.Fatal(err)
	}
	if err := s.PublishDuringDeviceSession(scoped, "session-b-watch", func() error { return nil }); err != nil {
		t.Fatalf("new App session watch was denied: %v", err)
	}
}
