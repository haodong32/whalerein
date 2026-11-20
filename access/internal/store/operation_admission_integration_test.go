//go:build integration

package store_test

import (
	"context"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
	"github.com/whalerein/whalerein/access/internal/store/storetest"
)

// An operation admitted before its authorization deadline may still commit
// after that deadline when its transaction is released. The admission row is
// visible while the mutation is blocked, and exactly one credential is issued.
func TestAdmittedOperationCommitsAfterItsAuthorizationDeadline(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	now := time.Now().UTC()
	expires := now.Add(2 * time.Second)

	operation := store.ExternalOperation{
		OperationID: "op-barrier", AuthorizationID: "auth-barrier",
		Purpose: "host_enrollment", WorkspaceID: "ws-barrier", RegionID: "region-1",
		HostID: "host-barrier", PayloadDigest: []byte("barrier-digest"),
		State: "operation_admitted", EnrollmentRevision: "1",
		AuthorizationExpiresAt: &expires,
	}
	admitted, created, err := s.AdmitOperation(ctx, operation)
	if err != nil || !created || admitted.Outcome != store.OperationAdmitted {
		t.Fatalf("admit = %+v created=%v err=%v", admitted, created, err)
	}

	tx, err := s.Pool().Begin(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = tx.Rollback(ctx) }()
	if _, err := tx.Exec(ctx, `SELECT pg_advisory_xact_lock(hashtextextended('whalerein-host:' || $1, 0))`, "host-barrier"); err != nil {
		t.Fatal(err)
	}

	type result struct {
		enrollment store.ExternalEnrollment
		credential string
		committed  bool
		err        error
	}
	done := make(chan result, 1)
	go func() {
		enrollment, credential, committed, err := s.EnrollHost(ctx, operation, store.ExternalEnrollment{
			HostID: "host-barrier", WorkspaceID: "ws-barrier", EnrollmentRevision: "1",
			HostSigningPublicKey: make([]byte, 32), HostNoisePublicKey: make([]byte, 32),
			State: "enrollment_pending",
		})
		done <- result{enrollment, credential, committed, err}
	}()

	// Let the mutation reach the Host lock, then expire the authorization.
	time.Sleep(500 * time.Millisecond)
	time.Sleep(time.Until(expires.Add(500 * time.Millisecond)))
	if err := tx.Commit(ctx); err != nil {
		t.Fatalf("release barrier: %v", err)
	}
	out := <-done
	if out.err != nil {
		t.Fatalf("commit after expiry: %v", out.err)
	}
	if !out.committed || out.credential == "" || out.enrollment.State != "enrollment_pending" {
		t.Fatalf("unexpected commit %+v credential=%q", out.enrollment, out.credential)
	}
}

// An operation admitted after its deadline is fenced without a mutation.
func TestOperationAdmittedPastTheDeadlineIsFencedWithoutMutation(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	past := time.Now().UTC().Add(-time.Minute)
	operation := store.ExternalOperation{
		OperationID: "op-late", AuthorizationID: "auth-late",
		Purpose: "host_enrollment", WorkspaceID: "ws-late", RegionID: "region-1",
		HostID: "host-late", PayloadDigest: []byte("late-digest"),
		State: "operation_admitted", EnrollmentRevision: "1",
		AuthorizationExpiresAt: &past,
	}
	if _, _, err := s.AdmitOperation(ctx, operation); err != nil {
		t.Fatal(err)
	}
	if _, _, _, err := s.EnrollHost(ctx, operation, store.ExternalEnrollment{
		HostID: "host-late", WorkspaceID: "ws-late", EnrollmentRevision: "1",
		HostSigningPublicKey: make([]byte, 32), HostNoisePublicKey: make([]byte, 32),
	}); err != store.ErrAuthorizationExpired {
		t.Fatalf("expected an expiry fence, got %v", err)
	}
	if _, err := s.Enrollment(ctx, "host-late"); err != store.ErrNotFound {
		t.Fatalf("a fenced operation mutated an enrollment: %v", err)
	}
}

func TestReplacementAdmittedBeforeDeadlineCommitsAfterExpiryAndReplaysWithoutASecret(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	now := time.Now().UTC()
	expires := now.Add(5 * time.Minute)
	enrollOp := store.ExternalOperation{
		OperationID: "op-replace-base", AuthorizationID: "auth-replace-base",
		Purpose: "host_enrollment", WorkspaceID: "ws-replace", RegionID: "region-1",
		HostID: "host-replace", PayloadDigest: []byte("enroll-digest"),
		State: "operation_admitted", EnrollmentRevision: "1",
		AuthorizationExpiresAt: &expires,
	}
	if _, _, err := s.AdmitOperation(ctx, enrollOp); err != nil {
		t.Fatal(err)
	}
	enrolled, credential, committed, err := s.EnrollHost(ctx, enrollOp, store.ExternalEnrollment{
		HostID: "host-replace", WorkspaceID: "ws-replace", EnrollmentRevision: "1",
		HostSigningPublicKey: make([]byte, 32), HostNoisePublicKey: make([]byte, 32),
		State: "enrollment_pending",
	})
	if err != nil || !committed || credential == "" || enrolled.State != "enrollment_pending" {
		t.Fatalf("seed enrollment = %+v %q %v %v", enrolled, credential, committed, err)
	}

	replaceExpiry := time.Now().UTC().Add(2 * time.Second)
	replaceOp := store.ExternalOperation{
		OperationID: "op-replace", AuthorizationID: "auth-replace",
		Purpose: "host_credential_recovery", WorkspaceID: "ws-replace", RegionID: "region-1",
		HostID: "host-replace", PayloadDigest: []byte("replace-digest"),
		State: "operation_admitted", EnrollmentRevision: "1",
		AuthorizationExpiresAt: &replaceExpiry,
	}
	if admitted, created, err := s.AdmitOperation(ctx, replaceOp); err != nil || !created || admitted.Outcome != store.OperationAdmitted {
		t.Fatalf("admit replace = %+v created=%v err=%v", admitted, created, err)
	}
	tx, err := s.Pool().Begin(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := tx.Exec(ctx, `SELECT pg_advisory_xact_lock(hashtextextended('whalerein-host:' || $1, 0))`, "host-replace"); err != nil {
		t.Fatal(err)
	}
	type result struct {
		enrollment store.ExternalEnrollment
		credential string
		err        error
	}
	done := make(chan result, 1)
	go func() {
		enrollment, credential, err := s.ReplaceHostCredential(ctx, replaceOp)
		done <- result{enrollment, credential, err}
	}()
	time.Sleep(500 * time.Millisecond)
	time.Sleep(time.Until(replaceExpiry.Add(500 * time.Millisecond)))
	if err := tx.Commit(ctx); err != nil {
		t.Fatal(err)
	}
	out := <-done
	if out.err != nil || out.credential == "" {
		t.Fatalf("replacement commit after expiry = %+v %q", out.enrollment, out.credential)
	}
	// The same operation after commit must return the recorded receipt without
	// a second credential.
	replayed, replayCredential, err := s.ReplaceHostCredential(ctx, replaceOp)
	if err != nil {
		t.Fatal(err)
	}
	if replayCredential != "" || replayed.CredentialGeneration != out.enrollment.CredentialGeneration {
		t.Fatalf("replay minted or changed a credential: %+v %q", replayed, replayCredential)
	}

	past := time.Now().UTC().Add(-time.Minute)
	late := store.ExternalOperation{
		OperationID: "op-replace-late", AuthorizationID: "auth-replace-late",
		Purpose: "host_credential_recovery", WorkspaceID: "ws-replace", RegionID: "region-1",
		HostID: "host-replace", PayloadDigest: []byte("replace-late"),
		State: "operation_admitted", EnrollmentRevision: "1",
		AuthorizationExpiresAt: &past,
	}
	if _, _, err := s.AdmitOperation(ctx, late); err != nil {
		t.Fatal(err)
	}
	if _, _, err := s.ReplaceHostCredential(ctx, late); err != store.ErrAuthorizationExpired {
		t.Fatalf("a late replacement admission must be fenced, got %v", err)
	}
	if stored, err := s.Enrollment(ctx, "host-replace"); err != nil || stored.CredentialGeneration != replayed.CredentialGeneration {
		t.Fatalf("a fenced replacement changed the credential: %+v %v", stored, err)
	}
}
