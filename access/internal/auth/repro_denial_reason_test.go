package auth

import (
	"context"
	"errors"
	"io"
	"log/slog"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/store"
)

// standaloneEnrollmentStore models the real *store.Store in Standalone mode:
// it implements the optional enrollmentReader capability, but Standalone has no
// external_enrollments rows, so any lookup is ErrNotFound.
type standaloneEnrollmentStore struct{ *fakeStore }

func (standaloneEnrollmentStore) Enrollment(context.Context, string) (store.ExternalEnrollment, error) {
	return store.ExternalEnrollment{}, store.ErrNotFound
}

// ACC-4 regression: Standalone denial reason.
//
// proto/v1/WIRE.md defines the authority reasons: grant_revoked means "This
// Device's grant for the Host was revoked", host_removed means "The Host
// enrollment no longer exists". Standalone never writes external enrollment
// rows, so an absent enrollment says nothing about the Host; a revoked grant
// must be reported as grant_revoked. host_removed is reserved for external
// mode, where a missing enrollment row means the Host was removed.
func TestReproStandaloneRevokedGrantReportsGrantRevokedReason(t *testing.T) {
	revoked := newFakeStore()
	revoked.hosts["h1"] = store.Host{HostID: "h1"}
	revoked.grants[grantKey("h1", "d1")] = false // grant revoked, Host still exists

	service, err := New(Dependencies{
		Store:     standaloneEnrollmentStore{revoked},
		Admission: newFakeAdmission(revoked),
		Enforcer:  &fakeEnforcer{},
		Presence:  &fakePresence{online: map[string]bool{}},
		Transport: &fakeTransport{},
		Config: Config{
			AdminToken: "admin-token", HostProvisioningToken: "host-token",
			AccessUsername: "access", AccessPassword: "access-pass",
			ProtocolVersion: 1, RevocationAckTimeout: time.Second,
		},
		Metrics: diagnostics.NewMetrics(),
		Masker:  diagnostics.NewMaskerWithKey([]byte("test")),
		Logger:  slog.New(slog.NewTextHandler(io.Discard, nil)),
	})
	if err != nil {
		t.Fatal(err)
	}

	authorizeErr := service.AuthorizeHost(context.Background(), Identity{DeviceID: "d1"}, "h1")
	var denial *AuthorityDenial
	if !errors.As(authorizeErr, &denial) {
		t.Fatalf("AuthorizeHost err = %v, want an *AuthorityDenial", authorizeErr)
	}
	if denial.StatusCode != 403 {
		t.Fatalf("status = %d, want 403", denial.StatusCode)
	}
	// The HTTP boundary copies this reason into ErrorResponse.reason verbatim.
	if denial.Reason != ReasonGrantRevoked {
		t.Fatalf("reason = %q, want %q: Standalone has no enrollment rows, so a "+
			"revoked grant is misreported as a removed Host enrollment",
			denial.Reason, ReasonGrantRevoked)
	}

	// Positive control: external mode requires an enrollment row, so the same
	// absent row means the Host was removed from the account service.
	service.cfg.RequireHostEnrollment = true
	externalErr := service.hostDenial(context.Background(), "h1")
	if !errors.As(externalErr, &denial) || denial.Reason != ReasonHostRemoved {
		t.Fatalf("external denial = %v, want reason %q", externalErr, ReasonHostRemoved)
	}
}
