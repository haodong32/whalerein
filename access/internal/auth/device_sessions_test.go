package auth

import (
	"bytes"
	"context"
	"errors"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
)

type managedAuthFixture struct {
	permit store.DeviceSession
	hosts  map[string]bool
	err    error
}

func (f *managedAuthFixture) DeviceSessionByCredential(_ context.Context, digest []byte, _ time.Time) (store.DeviceSession, error) {
	if !bytes.Equal(digest, f.permit.CredentialHash) {
		return store.DeviceSession{}, store.ErrNotFound
	}
	return f.permit, f.err
}

func (f *managedAuthFixture) ActiveDeviceSession(_ context.Context, id string, _ time.Time) (store.DeviceSession, error) {
	if id != f.permit.ID {
		return store.DeviceSession{}, store.ErrNotFound
	}
	return f.permit, f.err
}

func (f *managedAuthFixture) DeviceSessionHosts(context.Context, store.DeviceSession) (map[string]bool, error) {
	return f.hosts, f.err
}

// Managed HTTP and MQTT both require a current credential and the exact
// workspace's Host grants. Broker projection omits Device-wide presence.
func TestManagedAuthorizationScopesHTTPAndBroker(t *testing.T) {
	service, fs, _, _ := newService(t)
	ctx := context.Background()
	fixture := &managedAuthFixture{
		permit: store.DeviceSession{
			ID: "native-app-1", DeviceID: "device", AuthorityRevision: "revision",
			CredentialHash: HashToken("session token"), Revision: 3, ExpiresAt: time.Now().Add(time.Minute),
		},
		hosts: map[string]bool{"eligible": true},
	}
	service.sessions = fixture
	service.cfg.SessionAuthorityRevision = "revision"
	service.cfg.RequireDeviceSession = true
	for _, id := range []string{"eligible", "unrelated"} {
		fs.hosts[id] = store.Host{HostID: id}
		fs.grants[grantKey(id, "device")] = true
	}
	fs.devices["device"] = store.Device{DeviceID: "device", AuthHash: HashToken("legacy token")}
	if _, err := service.AuthenticateDeviceToken(ctx, "legacy token"); !errors.Is(err, ErrUnauthorized) {
		t.Fatalf("managed mode accepted old HTTP token: %v", err)
	}
	if _, err := service.AuthenticateBroker(ctx, "whalerein-device-device", "device:device", "legacy token"); !errors.Is(err, ErrUnauthorized) {
		t.Fatalf("managed mode accepted old MQTT token: %v", err)
	}
	identity, err := service.AuthenticateDeviceToken(ctx, "session token")
	if err != nil || identity.DeviceID != "device" || identity.SessionID != fixture.permit.ID {
		t.Fatalf("authentication = %+v %v", identity, err)
	}
	if err := service.AuthorizeHost(ctx, identity, "eligible"); err != nil {
		t.Fatal(err)
	}
	if err := service.AuthorizeHost(ctx, identity, "unrelated"); !errors.Is(err, ErrForbidden) {
		t.Fatalf("unrelated Host: %v", err)
	}
	if hosts, truncated, err := service.ListHosts(ctx, identity, 10, 0); err != nil || truncated ||
		len(hosts) != 1 || hosts[0].HostID != "eligible" {
		t.Fatalf("hosts=%+v truncated=%v err=%v", hosts, truncated, err)
	}
	brokerIdentity, err := service.AuthenticateBroker(ctx, "whalerein-session-native-app-1-live", "session:native-app-1", "session token")
	if err != nil || brokerIdentity.DeviceID != identity.DeviceID || brokerIdentity.SessionID != identity.SessionID {
		t.Fatalf("broker authentication = %+v %v", brokerIdentity, err)
	}
	if _, err := service.AuthenticateBroker(ctx, "whalerein-session-native-app-1-BAD", "session:native-app-1", "session token"); !errors.Is(err, ErrBadRequest) {
		t.Fatalf("incorrect client ID accepted: %v", err)
	}
	if _, err := service.AuthenticateBroker(ctx, "whalerein-session-unrelated", "session:unrelated", "session token"); !errors.Is(err, ErrUnauthorized) {
		t.Fatalf("credential accepted under another MQTT username: %v", err)
	}
	connection, err := service.BrokerPolicy(ctx, brokerIdentity)
	if err != nil || connection.SessionID != identity.SessionID || connection.Will != "" ||
		connection.SessionRevision != 3 || connection.SessionExpiresAt != fixture.permit.ExpiresAt.UnixMilli() ||
		len(connection.Entries) != 3 {
		t.Fatalf("connection=%+v err=%v", connection, err)
	}
	for _, entry := range connection.Entries {
		if entry.HostID != "eligible" {
			t.Fatalf("out of scope policy: %+v", entry)
		}
	}
	service.cfg.SessionAuthorityRevision = "new-revision"
	if _, err := service.AuthenticateDeviceToken(ctx, "session token"); !errors.Is(err, ErrUnauthorized) {
		t.Fatalf("old authority credential accepted: %v", err)
	}
	if policy, err := service.ManagedBrokerPolicy(ctx, identity.SessionID, identity.DeviceID); err != nil || policy.SessionID != "" {
		t.Fatalf("stale broker policy remained active: %+v %v", policy, err)
	}
	fixture.err = errors.New("database unavailable")
	if _, err := service.AuthenticateDeviceToken(ctx, "session token"); !errors.Is(err, fixture.err) {
		t.Fatalf("store failure classification: %v", err)
	}
	if _, _, err := service.ListHosts(ctx, identity, 10, 0); !errors.Is(err, fixture.err) {
		t.Fatalf("list store failure classification: %v", err)
	}
	if err := service.AuthorizeHost(ctx, identity, "eligible"); !errors.Is(err, fixture.err) {
		t.Fatalf("authorize store failure classification: %v", err)
	}
	if _, err := service.ManagedBrokerPolicy(ctx, identity.SessionID, identity.DeviceID); !errors.Is(err, fixture.err) {
		t.Fatalf("broker store failure classification: %v", err)
	}
}
