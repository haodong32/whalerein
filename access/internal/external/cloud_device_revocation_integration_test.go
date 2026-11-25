//go:build integration

package external_test

import (
	"bytes"
	"context"
	"errors"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/external"
	"github.com/whalerein/whalerein/access/internal/store"
	"github.com/whalerein/whalerein/access/internal/store/storetest"
)

// lostAckEnforcer applies the durable grant revocation, then reports the
// regional acknowledgement as lost. A retry finds the grant already enforced.
type lostAckEnforcer struct {
	data      *store.Store
	loseAck   bool
	calls     int
	effective int
}

func (e *lostAckEnforcer) EnforceRevocationScope(ctx context.Context, scope store.RevocationScope) (store.EnforcementReceipt, error) {
	e.calls++
	for _, grant := range scope.Grants {
		var revoked bool
		err := e.data.Pool().QueryRow(ctx, `SELECT COALESCE(revoked_at IS NOT NULL,true)
			FROM grants WHERE host_id=$1 AND device_id=$2`, grant.HostID, grant.DeviceID).Scan(&revoked)
		if err != nil {
			return store.EnforcementReceipt{Scope: scope}, err
		}
		if !revoked {
			e.effective++
			if err := e.data.RevokeGrant(ctx, grant.HostID, grant.DeviceID, time.Now().UTC()); err != nil {
				return store.EnforcementReceipt{Scope: scope}, err
			}
		}
	}
	if e.loseAck {
		e.loseAck = false
		return store.EnforcementReceipt{Scope: scope}, errors.New("simulated lost regional acknowledgement")
	}
	return store.EnforcementReceipt{Scope: scope, Absent: true}, nil
}

func enrollIntegrationHost(t *testing.T, data *store.Store, workspace, hostID, revision, operationID string, now time.Time) {
	t.Helper()
	ctx := context.Background()
	operation := store.ExternalOperation{
		OperationID: operationID, AuthorizationID: "auth-" + operationID,
		Purpose: external.PurposeHostEnrollment, WorkspaceID: workspace, RegionID: "region",
		HostID: hostID, PayloadDigest: []byte{1}, EnrollmentRevision: revision,
		State: "operation_committed",
	}
	if _, _, _, err := data.EnrollHost(ctx, operation, store.ExternalEnrollment{
		HostID: hostID, WorkspaceID: workspace, EnrollmentRevision: revision,
		State: "enrollment_pending", HostSigningPublicKey: bytes.Repeat([]byte{3}, 32),
		HostNoisePublicKey: bytes.Repeat([]byte{1}, 32),
	}); err != nil {
		t.Fatal(err)
	}
	if _, err := data.EnsureHost(ctx, hostID); err != nil {
		t.Fatal(err)
	}
	if _, advanced, err := data.MarkEnrollmentReady(ctx, hostID, now); err != nil || !advanced {
		t.Fatalf("enrollment %s did not become ready: %v", hostID, err)
	}
}

// A retry after a lost regional acknowledgement completes the same pending
// revocation once and never revokes the sibling Host grant.
func TestCloudDeviceRevocationRetryAfterLostAcknowledgementIsIdempotent(t *testing.T) {
	ctx := context.Background()
	data, _ := storetest.New(t)
	now := time.Now().UTC()
	const workspace, device = "workspace", "device"
	enrollIntegrationHost(t, data, workspace, "host-a", "1", "enroll-a", now)
	enrollIntegrationHost(t, data, workspace, "host-b", "1", "enroll-b", now)
	for _, host := range []string{"host-a", "host-b"} {
		if err := data.CreatePairedDevice(ctx, host, device, "fingerprint", bytes.Repeat([]byte{8}, 32), now); err != nil {
			t.Fatal(err)
		}
	}

	enforcer := &lostAckEnforcer{data: data, loseAck: true}
	service := external.New(external.Trust{}, data, "", external.WithDeviceGrants(nil, data, enforcer))
	if err := service.RevokeDeviceGrant(ctx, workspace, "host-a", device); !errors.Is(err, external.ErrEnforcementUnavailable) {
		t.Fatalf("lost acknowledgement should stay pending, got %v", err)
	}
	if enforcer.calls != 1 || enforcer.effective != 1 {
		t.Fatalf("first enforcement calls=%d effective=%d", enforcer.calls, enforcer.effective)
	}
	if state, err := data.CloudDeviceGrantStatus(ctx, workspace, "host-a", device); err != nil || state != "pending" {
		t.Fatalf("pending status = %q, err=%v", state, err)
	}
	if revoked, err := grantRevoked(ctx, data, "host-b", device); err != nil || revoked {
		t.Fatalf("sibling grant was revoked by the first attempt: %v, %v", revoked, err)
	}

	// The retry reuses the durable fence recorded by the first attempt and
	// enforces the grant exactly once.
	if err := service.RevokeDeviceGrant(ctx, workspace, "host-a", device); err != nil {
		t.Fatalf("retry could not complete enforcement: %v", err)
	}
	if enforcer.calls != 2 || enforcer.effective != 1 {
		t.Fatalf("retry enforcement calls=%d effective=%d", enforcer.calls, enforcer.effective)
	}
	if state, err := data.CloudDeviceGrantStatus(ctx, workspace, "host-a", device); err != nil || state != "revoked" {
		t.Fatalf("enforced status = %q, err=%v", state, err)
	}
	if revoked, err := grantRevoked(ctx, data, "host-a", device); err != nil || !revoked {
		t.Fatalf("target grant not revoked: %v, %v", revoked, err)
	}
	if revoked, err := grantRevoked(ctx, data, "host-b", device); err != nil || revoked {
		t.Fatalf("sibling grant did not survive the retry: %v, %v", revoked, err)
	}
}

func grantRevoked(ctx context.Context, data *store.Store, hostID, deviceID string) (bool, error) {
	var revoked bool
	err := data.Pool().QueryRow(ctx, `SELECT COALESCE(revoked_at IS NOT NULL,true)
		FROM grants WHERE host_id=$1 AND device_id=$2`, hostID, deviceID).Scan(&revoked)
	return revoked, err
}
