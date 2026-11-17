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
)

// When HostReady is consumed for an enrolled Host, the enrollment advances to
// ready and the operation outcome is recorded in the same transaction.
func TestExternalLifecycleReadinessAdvancesAtomically(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	now := time.Now().UTC()
	operation := store.ExternalOperation{
		OperationID: "op-ready", AuthorizationID: "auth-ready",
		Purpose: "host_enrollment", WorkspaceID: "ws-1", RegionID: "region-1",
		HostID: "host-ready", PayloadDigest: []byte("digest"),
		State: "operation_committed", EnrollmentRevision: "1",
	}
	if _, _, _, err := s.EnrollHost(ctx, operation, store.ExternalEnrollment{
		HostID: "host-ready", WorkspaceID: "ws-1", EnrollmentRevision: "1",
		HostSigningPublicKey: make([]byte, 32), HostNoisePublicKey: make([]byte, 32),
		State: "enrollment_pending",
	}); err != nil {
		t.Fatal(err)
	}
	enrollment, advanced, err := s.MarkEnrollmentReady(ctx, "host-ready", now)
	if err != nil || !advanced || enrollment.State != "enrollment_ready" {
		t.Fatalf("ready = %+v advanced=%v err=%v", enrollment, advanced, err)
	}
	var outcome string
	if err := s.Pool().QueryRow(ctx,
		`SELECT outcome FROM external_operations WHERE operation_id = $1`, "op-ready").Scan(&outcome); err != nil {
		t.Fatal(err)
	}
	if outcome != "enrollment_ready" {
		t.Fatalf("operation outcome = %q", outcome)
	}
	if _, advanced, err := s.MarkEnrollmentReady(ctx, "host-ready", now); err != nil || advanced {
		t.Fatalf("a repeated readiness must be a no-op: advanced=%v err=%v", advanced, err)
	}
	if _, _, err := s.MarkEnrollmentReady(ctx, "host-unknown", now); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("expected ErrNotFound, got %v", err)
	}
}

// When one enrollment is disabled, its live grants are revoked in the same
// transaction that records disabled_at, and repeating the call is idempotent.
func TestExternalLifecycleDisableRevokesGrantsIdempotently(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	now := time.Now().UTC()
	if _, err := s.EnsureHost(ctx, "host-disable"); err != nil {
		t.Fatal(err)
	}
	authHash := sha256.Sum256([]byte("device-token"))
	if err := s.CreatePairedDevice(ctx, "host-disable", "device-disable", "pin", authHash[:], now); err != nil {
		t.Fatal(err)
	}
	operation := store.ExternalOperation{
		OperationID: "op-disable", AuthorizationID: "auth-disable",
		Purpose: "host_enrollment", WorkspaceID: "ws-1", RegionID: "region-1",
		HostID: "host-disable", PayloadDigest: []byte("digest"),
		State: "operation_committed", EnrollmentRevision: "1",
	}
	if _, _, _, err := s.EnrollHost(ctx, operation, store.ExternalEnrollment{
		HostID: "host-disable", WorkspaceID: "ws-1", EnrollmentRevision: "1",
		HostSigningPublicKey: make([]byte, 32), HostNoisePublicKey: make([]byte, 32),
		State: "enrollment_pending",
	}); err != nil {
		t.Fatal(err)
	}
	disabled, err := s.DisableEnrollment(ctx, "host-disable", now)
	if err != nil {
		t.Fatal(err)
	}
	if !disabled.DisabledAt.Equal(now) || disabled.Revocations["device-disable"] == 0 {
		t.Fatalf("disable = %+v", disabled)
	}
	if active, err := s.HasActiveGrant(ctx, "host-disable", "device-disable"); err != nil || active {
		t.Fatalf("the grant survived disable: active=%v err=%v", active, err)
	}
	repeated, err := s.DisableEnrollment(ctx, "host-disable", now.Add(time.Minute))
	if err != nil {
		t.Fatal(err)
	}
	if !repeated.DisabledAt.Equal(now) || len(repeated.Revocations) != 0 {
		t.Fatalf("repeated disable = %+v", repeated)
	}
	if _, err := s.DisableEnrollment(ctx, "host-unknown", now); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("expected ErrNotFound, got %v", err)
	}
}

// When a credential is replaced, the stored hash changes and replaying the
// same authorization id cannot mint a second credential.
func TestExternalLifecycleCredentialReplacementIsOneTime(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	operation := store.ExternalOperation{
		OperationID: "op-recover", AuthorizationID: "auth-recover",
		Purpose: "host_enrollment", WorkspaceID: "ws-1", RegionID: "region-1",
		HostID: "host-recover", PayloadDigest: []byte("digest"),
		State: "operation_committed", EnrollmentRevision: "1",
	}
	_, initial, _, err := s.EnrollHost(ctx, operation, store.ExternalEnrollment{
		HostID: "host-recover", WorkspaceID: "ws-1", EnrollmentRevision: "1",
		HostSigningPublicKey: make([]byte, 32), HostNoisePublicKey: make([]byte, 32),
		State: "enrollment_pending",
	})
	if err != nil {
		t.Fatal(err)
	}
	recorded, replacement, err := s.ReplaceHostCredential(ctx, store.ExternalOperation{
		OperationID: "op-recover-2", AuthorizationID: "auth-recover-2",
		Purpose: "host_credential_recovery", WorkspaceID: "ws-1", RegionID: "region-1",
		HostID: "host-recover", PayloadDigest: []byte("recovery-digest"),
		State: "operation_committed", EnrollmentRevision: "1",
	})
	if err != nil {
		t.Fatal(err)
	}
	if replacement == "" || replacement == initial || recorded.HostID != "host-recover" {
		t.Fatalf("replacement = %+v credential=%q initial=%q", recorded, replacement, initial)
	}
	credential, err := s.HostBrokerCredential(ctx, "host-recover")
	if err != nil {
		t.Fatal(err)
	}
	sum := sha256.Sum256([]byte(replacement))
	if string(credential.BrokerCredentialHash) != string(sum[:]) {
		t.Fatal("the stored hash was not replaced")
	}
	if _, _, err := s.ReplaceHostCredential(ctx, store.ExternalOperation{
		OperationID: "op-recover-3", AuthorizationID: "auth-recover-2",
		Purpose: "host_credential_recovery", WorkspaceID: "ws-1", RegionID: "region-1",
		HostID: "host-recover", PayloadDigest: []byte("recovery-digest"),
		State: "operation_committed", EnrollmentRevision: "1",
	}); !errors.Is(err, store.ErrOperationConflict) {
		t.Fatalf("expected a replay conflict, got %v", err)
	}
}

// When a Host proof nonce is consumed twice, only the first call succeeds.
func TestExternalLifecycleHostProofNonceIsSingleUse(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	nonce := sha256.Sum256([]byte("nonce"))
	consumed, err := s.ConsumeHostProofNonce(ctx, nonce[:], "enrollment_status", time.Now().UTC())
	if err != nil || !consumed {
		t.Fatalf("first consume = %v, %v", consumed, err)
	}
	consumed, err = s.ConsumeHostProofNonce(ctx, nonce[:], "enrollment_status", time.Now().UTC())
	if err != nil || consumed {
		t.Fatalf("replayed consume = %v, %v", consumed, err)
	}
}
