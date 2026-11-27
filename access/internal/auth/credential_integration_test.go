//go:build integration

package auth

import (
	"context"
	"io"
	"log/slog"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/store"
	"github.com/whalerein/whalerein/access/internal/store/storetest"
)

// When an authorized enrollment is committed, the real broker admission path
// accepts only its per-enrollment credential; the shared provisioning token and
// a disabled enrollment are denied.
func TestAuthenticateBrokerUsesARealEnrollmentCredentialHash(t *testing.T) {
	data, _ := storetest.NewSQLite(t)
	ctx := context.Background()
	enrollment, credential, committed, err := data.EnrollHost(ctx, store.ExternalOperation{
		OperationID: "op-admission-2", AuthorizationID: "auth-admission-2",
		Purpose: "host_enrollment", WorkspaceID: "ws-1", RegionID: "region-1",
		HostID: "host-admission-2", PayloadDigest: []byte("digest-2"),
		State: "operation_committed", EnrollmentRevision: "2",
	}, store.ExternalEnrollment{
		HostID: "host-admission-2", WorkspaceID: "ws-1", EnrollmentRevision: "2",
		HostSigningPublicKey: make([]byte, 32), HostNoisePublicKey: make([]byte, 32),
		State: "enrollment_pending",
	})
	if err != nil || !committed || credential == "" || enrollment.HostID != "host-admission-2" {
		t.Fatalf("enrollment = %+v credential=%q committed=%v err=%v", enrollment, credential, committed, err)
	}
	fs := newFakeStore()
	service, err := New(Dependencies{
		Store:     data,
		Admission: newFakeAdmission(fs),
		Enforcer:  &fakeEnforcer{},
		Presence:  &fakePresence{online: map[string]bool{}},
		Transport: &fakeTransport{},
		Config: Config{
			HostProvisioningToken: "shared-token",
			RequireHostEnrollment: true,
		},
		Metrics: diagnostics.NewMetrics(),
		Masker:  diagnostics.NewMaskerWithKey([]byte("test")),
		Logger:  slog.New(slog.NewTextHandler(io.Discard, nil)),
	})
	if err != nil {
		t.Fatal(err)
	}
	identity, err := service.AuthenticateBroker(ctx, "whalerein-host-host-admission-2", "host:host-admission-2", credential)
	if err != nil || identity.Kind != KindHost || identity.HostID != "host-admission-2" {
		t.Fatalf("identity = %+v err = %v", identity, err)
	}
	if _, err := data.GetHost(ctx, "host-admission-2"); err != nil {
		t.Fatalf("the Host projection row is missing: %v", err)
	}
	if _, err := service.AuthenticateBroker(ctx, "whalerein-host-host-admission-2", "host:host-admission-2", "shared-token"); err != ErrUnauthorized {
		t.Fatalf("the shared token must be denied for a managed enrollment: %v", err)
	}
	if _, err := service.AuthenticateBroker(ctx, "whalerein-host-unenrolled", "host:unenrolled", "shared-token"); err != ErrUnauthorized {
		t.Fatalf("an unenrolled Host must be denied in external mode: %v", err)
	}
	if _, err := data.DisableEnrollment(ctx, "host-admission-2", time.Now().UTC()); err != nil {
		t.Fatal(err)
	}
	if _, err := service.AuthenticateBroker(ctx, "whalerein-host-host-admission-2", "host:host-admission-2", credential); err != ErrUnauthorized {
		t.Fatalf("a disabled enrollment must be denied: %v", err)
	}
}
