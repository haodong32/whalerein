package external

import (
	"context"
	"crypto/sha256"
	"errors"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

type sessionFixture struct {
	current     store.DeviceSession
	endID       string
	endRevision int64
	refusal     error
}

func (f *sessionFixture) InstallDeviceSession(_ context.Context, permission store.DeviceSession) error {
	f.current = permission
	return f.refusal
}

func (f *sessionFixture) RevokeDeviceSession(_ context.Context, id string, rev int64) error {
	f.endID, f.endRevision = id, rev
	return f.refusal
}

func (f *sessionFixture) DeviceSessionByCredential(_ context.Context, digest []byte, _ time.Time) (store.DeviceSession, error) {
	if string(digest) != string(f.current.CredentialHash) {
		return store.DeviceSession{}, store.ErrNotFound
	}
	return f.current, f.refusal
}

func (f *sessionFixture) DeviceSessionPairingStatus(_ context.Context, permission store.DeviceSession, _, _ string) (string, error) {
	if permission.ID != f.current.ID {
		return "", store.ErrNotFound
	}
	return "terminal", nil
}

type sessionEnforcerFixture struct {
	renewed store.DeviceSession
	ended   string
	refusal error
}

func (f *sessionEnforcerFixture) RenewDeviceSession(_ context.Context, permission store.DeviceSession) error {
	f.renewed = permission
	return f.refusal
}

func (f *sessionEnforcerFixture) EndDeviceSession(_ context.Context, id string) error {
	f.ended = id
	return f.refusal
}

// The issuer's Device permit binds the environment, authority, regional
// digest and session before either durable install or broker refresh.
func TestDeviceSessionPermitVerifiesEveryScopeBeforeInstall(t *testing.T) {
	const sessionID, env, authority = "session-a", "local", "rev-1"
	signer := newIssuer(t, "region-key", issued.Add(-time.Hour), issued.Add(time.Hour))
	trust := trustFor(signer)
	trust.SessionEnvironment, trust.SessionAuthorityRevision = env, authority
	data := &sessionFixture{}
	broker := &sessionEnforcerFixture{}
	service := &Service{trust: trust, now: func() time.Time { return issued }}
	WithDeviceSessions(data, broker, true)(service)
	ctx := context.Background()
	digest := sha256.Sum256([]byte("fixture credentials"))
	authorization := func() *whalereinv1.ExternalAuthorization {
		a := baseAuth(trust.Issuer)
		a.Purpose, a.OperationId = PurposeDeviceSession, sessionID
		a.HostId, a.Precondition = "", env
		a.EnrollmentRevision, a.ExpectedRevision = authority, "2"
		a.DeviceId, a.DevicePublicKeys = "device-a", [][]byte{make([]byte, 32)}
		a.Nonce = digest[:]
		return a
	}

	for _, mutation := range []struct {
		name   string
		change func(*whalereinv1.ExternalAuthorization)
	}{
		{"authority", func(a *whalereinv1.ExternalAuthorization) { a.EnrollmentRevision = "stale" }},
		{"environment", func(a *whalereinv1.ExternalAuthorization) { a.Precondition = "other" }},
		{"session", func(a *whalereinv1.ExternalAuthorization) { a.OperationId = "other" }},
		{"digest", func(a *whalereinv1.ExternalAuthorization) { a.Nonce = []byte("short") }},
		{"duration", func(a *whalereinv1.ExternalAuthorization) { a.ExpiresAtUnixMs++ }},
		{"issue-time", func(a *whalereinv1.ExternalAuthorization) { a.IssuedAtUnixMs++ }},
		{"revision", func(a *whalereinv1.ExternalAuthorization) { a.ExpectedRevision = "0" }},
		{"Device key", func(a *whalereinv1.ExternalAuthorization) { a.DevicePublicKeys = nil }},
		{"Host scope", func(a *whalereinv1.ExternalAuthorization) { a.HostId = "host" }},
	} {
		t.Run(mutation.name, func(t *testing.T) {
			a := authorization()
			mutation.change(a)
			if err := service.InstallDeviceSession(ctx, sessionID, signer.authorize(t, a)); !errors.Is(err, ErrAuthorizationMismatch) {
				t.Fatalf("successful or misclassified validation: %v", err)
			}
			if data.current.ID != "" || broker.renewed.ID != "" {
				t.Fatal("invalid permit reached a side effect")
			}
		})
	}
	a := signer.authorize(t, authorization())
	unconfigured := &Service{}
	if err := unconfigured.InstallDeviceSession(ctx, sessionID, a); !errors.Is(err, ErrEnforcementUnavailable) {
		t.Fatalf("unconfigured permission issuer: %v", err)
	}
	if err := unconfigured.RevokeDeviceSession(ctx, sessionID, 2); !errors.Is(err, ErrEnforcementUnavailable) {
		t.Fatalf("unconfigured revocation issuer: %v", err)
	}
	data.refusal = errors.New("regional storage unavailable")
	if err := service.InstallDeviceSession(ctx, sessionID, a); !errors.Is(err, data.refusal) || broker.renewed.ID != "" {
		t.Fatalf("failed persistence reached broker policy: %v", err)
	}
	data.refusal = nil
	if err := service.InstallDeviceSession(ctx, sessionID, a); err != nil {
		t.Fatal(err)
	}
	if data.current.ID != sessionID || data.current.Revision != 2 || broker.renewed.ID != sessionID ||
		broker.renewed.Revision != 2 || !broker.renewed.ExpiresAt.Equal(expires) {
		t.Fatal("validation did not persist and project the same session and revision")
	}
	broker.refusal = errors.New("broker unavailable")
	if err := service.InstallDeviceSession(ctx, sessionID, a); !errors.Is(err, ErrEnforcementUnavailable) {
		t.Fatalf("projection failure: %v", err)
	}
	if state, err := service.DeviceSessionPairingStatus(ctx, "fixture credentials", "host-a", "op-a"); err != nil || state != "terminal" {
		t.Fatalf("valid permit status=%q err=%v", state, err)
	}
	if _, err := service.DeviceSessionPairingStatus(ctx, "wrong fixture credential", "host-a", "op-a"); !errors.Is(err, ErrAuthorizationMismatch) {
		t.Fatalf("wrong credential status: %v", err)
	}
}

// Logout keeps the durable regional fence even when the connected broker
// remains unavailable and applies only to the original session.
func TestDeviceSessionRevocationPersistsBeforeBrokerAcknowledgment(t *testing.T) {
	data := &sessionFixture{}
	broker := &sessionEnforcerFixture{refusal: errors.New("broker unavailable")}
	service := &Service{}
	WithDeviceSessions(data, broker, true)(service)
	ctx := context.Background()
	if err := service.RevokeDeviceSession(ctx, "session-a", 0); !errors.Is(err, ErrAuthorizationMismatch) {
		t.Fatalf("invalid revision: %v", err)
	}
	if err := service.RevokeDeviceSession(ctx, "session-a", 3); !errors.Is(err, ErrEnforcementUnavailable) {
		t.Fatalf("expected pending enforcement: %v", err)
	}
	if data.endID != "session-a" || data.endRevision != 3 || broker.ended != data.endID {
		t.Fatal("wrong session revocation fence or broker scope")
	}
	broker.refusal = nil
	if err := service.RevokeDeviceSession(ctx, "session-a", 3); err != nil {
		t.Fatalf("retry could not confirm enforcement: %v", err)
	}
}
