package auth

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"log/slog"
	"strings"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

type fakeStore struct {
	hosts        map[string]store.Host
	devices      map[string]store.Device
	grants       map[string]bool // host|device
	keys         map[string][]byte
	keyErr       error
	revokeErr    error
	pending      []store.Revocation
	pendingErr   error
	markErr      error
	markCalls    []string
	revokeHosts  []string
	revokeExists bool
	ensureErr    error
	activeErr    error
	grantErr     error
}

func newFakeStore() *fakeStore {
	return &fakeStore{
		hosts:   map[string]store.Host{},
		devices: map[string]store.Device{},
		grants:  map[string]bool{},
		keys:    map[string][]byte{},
	}
}

func grantKey(hostID, deviceID string) string { return hostID + "|" + deviceID }

func (f *fakeStore) EnsureHost(_ context.Context, hostID string) (store.Host, error) {
	if f.ensureErr != nil {
		return store.Host{}, f.ensureErr
	}
	host, ok := f.hosts[hostID]
	if !ok {
		host = store.Host{HostID: hostID}
		f.hosts[hostID] = host
	}
	return host, nil
}

func (f *fakeStore) GetHost(_ context.Context, hostID string) (store.Host, error) {
	host, ok := f.hosts[hostID]
	if !ok {
		return store.Host{}, store.ErrNotFound
	}
	return host, nil
}

func (f *fakeStore) ListAuthorizedHosts(_ context.Context, deviceID string) ([]store.Host, error) {
	var out []store.Host
	for key, active := range f.grants {
		if !active {
			continue
		}
		parts := strings.Split(key, "|")
		if len(parts) == 2 && parts[1] == deviceID {
			out = append(out, f.hosts[parts[0]])
		}
	}
	return out, nil
}

func (f *fakeStore) ActiveDeviceByAuthHash(_ context.Context, authHash []byte) (store.Device, error) {
	if f.activeErr != nil {
		return store.Device{}, f.activeErr
	}
	for _, device := range f.devices {
		if bytes.Equal(device.AuthHash, authHash) && f.hasGrant(device.DeviceID) {
			return device, nil
		}
	}
	return store.Device{}, store.ErrNotFound
}

func (f *fakeStore) hasGrant(deviceID string) bool {
	for key, active := range f.grants {
		if active && strings.HasSuffix(key, "|"+deviceID) {
			return true
		}
	}
	return false
}

func (f *fakeStore) DeviceByID(_ context.Context, deviceID string) (store.Device, error) {
	device, ok := f.devices[deviceID]
	if !ok {
		return store.Device{}, store.ErrNotFound
	}
	return device, nil
}

func (f *fakeStore) HasActiveGrant(_ context.Context, hostID, deviceID string) (bool, error) {
	if f.grantErr != nil {
		return false, f.grantErr
	}
	return f.grants[grantKey(hostID, deviceID)], nil
}

func (f *fakeStore) RevokeDeviceGrants(_ context.Context, deviceID string, at time.Time) ([]string, bool, error) {
	if f.revokeErr != nil {
		return nil, false, f.revokeErr
	}
	if _, ok := f.devices[deviceID]; !ok {
		return nil, false, nil
	}
	hosts := []string{}
	for key, active := range f.grants {
		if !active {
			continue
		}
		parts := strings.Split(key, "|")
		if len(parts) == 2 && parts[1] == deviceID {
			hosts = append(hosts, parts[0])
			f.grants[key] = false
			f.pending = append(f.pending, store.Revocation{HostID: parts[0], DeviceID: deviceID, RevokedAt: at})
		}
	}
	f.revokeHosts = hosts
	f.revokeExists = true
	return hosts, true, nil
}

func (f *fakeStore) SetHostAuthorizationKey(_ context.Context, hostID string, publicKey []byte) error {
	if f.keyErr != nil {
		return f.keyErr
	}
	if _, ok := f.hosts[hostID]; !ok {
		return store.ErrNotFound
	}
	if _, ok := f.keys[hostID]; ok {
		return nil
	}
	f.keys[hostID] = append([]byte(nil), publicKey...)
	return nil
}

func (f *fakeStore) HostAuthorizationKey(_ context.Context, hostID string) ([]byte, error) {
	if f.keyErr != nil {
		return nil, f.keyErr
	}
	key, ok := f.keys[hostID]
	if !ok || len(key) != 32 {
		return nil, store.ErrNotFound
	}
	return append([]byte(nil), key...), nil
}

func (f *fakeStore) RevokeGrant(_ context.Context, hostID, deviceID string, _ time.Time) error {
	key := grantKey(hostID, deviceID)
	if _, ok := f.grants[key]; !ok {
		return store.ErrNotFound
	}
	f.grants[key] = false
	return nil
}

func (f *fakeStore) PendingRevocations(_ context.Context, hostID string) ([]store.Revocation, error) {
	if f.pendingErr != nil {
		return nil, f.pendingErr
	}
	var out []store.Revocation
	for _, item := range f.pending {
		if item.HostID == "" || item.HostID == hostID {
			out = append(out, item)
		}
	}
	return out, nil
}

func (f *fakeStore) MarkRevocationsReplayed(_ context.Context, hostID string, deviceIDs []string, _, _ time.Time, _ map[string]uint64) error {
	if f.markErr != nil {
		return f.markErr
	}
	f.markCalls = append(f.markCalls, hostID+":"+strings.Join(deviceIDs, ","))
	marked := map[string]bool{}
	for _, id := range deviceIDs {
		marked[id] = true
	}
	remaining := f.pending[:0]
	for _, item := range f.pending {
		if !marked[item.DeviceID] {
			remaining = append(remaining, item)
		}
	}
	f.pending = remaining
	return nil
}

type fakePresence struct {
	online map[string]bool
	err    error
}

func (f *fakePresence) IsHostOnline(_ context.Context, hostID string) bool {
	if f.err != nil {
		return false
	}
	return f.online[hostID]
}

type fakeTransport struct {
	publishErr    error
	awaitErr      error
	disconnectErr error
	published     []*whalereinv1.HostRequest
	awaited       []*whalereinv1.HostRequest
	disconnected  []string
	awaitFn       func(*whalereinv1.HostRequest) (*whalereinv1.HostResult, error)
}

func (f *fakeTransport) PublishAndAwait(_ context.Context, _ string, req *whalereinv1.HostRequest, _ time.Duration) (*whalereinv1.HostResult, error) {
	f.awaited = append(f.awaited, req)
	if f.awaitErr != nil {
		return nil, f.awaitErr
	}
	if f.awaitFn != nil {
		return f.awaitFn(req)
	}
	return nil, errors.New("no await function")
}

func (f *fakeTransport) Publish(_ context.Context, _ string, req *whalereinv1.HostRequest) error {
	if f.publishErr != nil {
		return f.publishErr
	}
	f.published = append(f.published, req)
	return nil
}

func (f *fakeTransport) DisconnectDevice(_ context.Context, deviceID string) error {
	f.disconnected = append(f.disconnected, deviceID)
	return f.disconnectErr
}

func newService(t *testing.T) (*Service, *fakeStore, *fakePresence, *fakeTransport) {
	t.Helper()
	fs := newFakeStore()
	presence := &fakePresence{online: map[string]bool{}}
	transport := &fakeTransport{}
	logger := slog.New(slog.NewTextHandler(io.Discard, nil))
	service, err := New(Dependencies{
		Store: fs, Presence: presence, Transport: transport,
		Config: Config{
			AdminToken: "admin-token", HostProvisioningToken: "host-token",
			AccessUsername: "access", AccessPassword: "access-pass",
			ProtocolVersion: 1, RevocationAckTimeout: time.Second,
		},
		Metrics: diagnostics.NewMetrics(),
		Masker:  diagnostics.NewMaskerWithKey([]byte("test")),
		Logger:  logger,
	})
	if err != nil {
		t.Fatal(err)
	}
	return service, fs, presence, transport
}

func TestNewRejectsMissingRequiredDependencies(t *testing.T) {
	if _, err := New(Dependencies{}); err == nil {
		t.Fatal("expected error")
	}
	if _, err := New(Dependencies{Store: newFakeStore()}); err == nil {
		t.Fatal("expected presence error")
	}
	if _, err := New(Dependencies{Store: newFakeStore(), Presence: &fakePresence{}}); err == nil {
		t.Fatal("expected transport error")
	}
}

func TestTokenHashingAuthenticatesAdminAndRejectsInvalidTokens(t *testing.T) {
	if len(HashToken("token")) != 32 {
		t.Fatal("hash length")
	}
	if !tokenEqual("a", "a") || tokenEqual("a", "b") {
		t.Fatal("tokenEqual")
	}
	service, _, _, _ := newService(t)
	if err := service.AuthenticateAdmin("admin-token"); err != nil {
		t.Fatal(err)
	}
	if err := service.AuthenticateAdmin("wrong"); !errors.Is(err, ErrUnauthorized) {
		t.Fatalf("err = %v", err)
	}
	if err := service.AuthenticateAdmin(""); !errors.Is(err, ErrUnauthorized) {
		t.Fatalf("empty err = %v", err)
	}
}

func TestDeviceAuthenticationReturnsOnlyGrantedHosts(t *testing.T) {
	service, fs, presence, _ := newService(t)
	ctx := context.Background()
	hash := HashToken("device-token")
	fs.devices["d1"] = store.Device{DeviceID: "d1", AuthHash: hash}
	fs.hosts["h1"] = store.Host{HostID: "h1", Fingerprint: "fp", ProtocolVersion: 1}
	fs.grants[grantKey("h1", "d1")] = true
	presence.online["h1"] = true

	identity, err := service.AuthenticateDeviceToken(ctx, "device-token")
	if err != nil || identity.DeviceID != "d1" {
		t.Fatalf("identity = %+v err=%v", identity, err)
	}
	if _, err := service.AuthenticateDeviceToken(ctx, ""); !errors.Is(err, ErrUnauthorized) {
		t.Fatalf("empty err = %v", err)
	}
	if _, err := service.AuthenticateDeviceToken(ctx, "wrong"); !errors.Is(err, ErrUnauthorized) {
		t.Fatalf("wrong err = %v", err)
	}
	fs.activeErr = errors.New("db down")
	if _, err := service.AuthenticateDeviceToken(ctx, "device-token"); err == nil || errors.Is(err, ErrUnauthorized) {
		t.Fatalf("store error = %v", err)
	}
	fs.activeErr = nil

	if err := service.AuthorizeHost(ctx, identity, "h1"); err != nil {
		t.Fatal(err)
	}
	if err := service.AuthorizeHost(ctx, identity, "h2"); !errors.Is(err, ErrForbidden) {
		t.Fatalf("forbidden err = %v", err)
	}
	hosts, err := service.ListHosts(ctx, identity)
	if err != nil || len(hosts) != 1 || !hosts[0].Online || hosts[0].Fingerprint != "fp" {
		t.Fatalf("hosts = %+v err=%v", hosts, err)
	}
	fs.grants[grantKey("h1", "d1")] = false
	if err := service.AuthorizeHost(ctx, identity, "h1"); !errors.Is(err, ErrForbidden) {
		t.Fatalf("revoked err = %v", err)
	}
}

func TestIdentityFromCredentialsRejectsMalformedPrincipals(t *testing.T) {
	service, _, _, _ := newService(t)
	cases := []struct {
		clientID string
		username string
		kind     PrincipalKind
		hostID   string
		deviceID string
		wantErr  bool
	}{
		{"anything", "access", KindAccess, "", "", false},
		{"", "access", "", "", "", true},
		{"whalerein-host-h1", "host:h1", KindHost, "h1", "", false},
		{"wrong", "host:h1", "", "", "", true},
		{"whalerein-device-d1", "device:d1", KindDevice, "", "d1", false},
		{"whalerein-device-d1", "device:d2", "", "", "", true},
		{"x", "other", "", "", "", true},
	}
	for _, tc := range cases {
		identity, err := service.IdentityFromCredentials(tc.clientID, tc.username)
		if (err != nil) != tc.wantErr {
			t.Fatalf("%q/%q err = %v", tc.clientID, tc.username, err)
		}
		if err == nil && (identity.Kind != tc.kind || identity.HostID != tc.hostID || identity.DeviceID != tc.deviceID) {
			t.Fatalf("%q/%q identity = %+v", tc.clientID, tc.username, identity)
		}
	}
}

func TestAuthenticateBrokerAllowsProvisionedHostsAndGrantedDevices(t *testing.T) {
	service, fs, _, _ := newService(t)
	ctx := context.Background()
	if _, err := service.AuthenticateBroker(ctx, "access-client", "access", "access-pass"); err != nil {
		t.Fatal(err)
	}
	if _, err := service.AuthenticateBroker(ctx, "access-client", "access", "wrong"); !errors.Is(err, ErrUnauthorized) {
		t.Fatalf("access wrong = %v", err)
	}
	if _, err := service.AuthenticateBroker(ctx, "whalerein-host-h1", "host:h1", "host-token"); err != nil {
		t.Fatal(err)
	}
	if _, ok := fs.hosts["h1"]; !ok {
		t.Fatal("host not provisioned")
	}
	if _, err := service.AuthenticateBroker(ctx, "whalerein-host-h1", "host:h1", "wrong"); !errors.Is(err, ErrUnauthorized) {
		t.Fatalf("host wrong = %v", err)
	}
	fs.ensureErr = errors.New("db down")
	if _, err := service.AuthenticateBroker(ctx, "whalerein-host-h2", "host:h2", "host-token"); err == nil {
		t.Fatal("expected ensure host error")
	}
	fs.ensureErr = nil

	fs.devices["d1"] = store.Device{DeviceID: "d1", AuthHash: HashToken("device-token")}
	fs.grants[grantKey("h1", "d1")] = true
	if _, err := service.AuthenticateBroker(ctx, "whalerein-device-d1", "device:d1", "device-token"); err != nil {
		t.Fatal(err)
	}
	if _, err := service.AuthenticateBroker(ctx, "whalerein-device-d1", "device:d1", "wrong"); !errors.Is(err, ErrUnauthorized) {
		t.Fatalf("device wrong = %v", err)
	}
	fs.devices["d2"] = store.Device{DeviceID: "d2", AuthHash: HashToken("device-2")}
	fs.grants[grantKey("h1", "d2")] = true
	if _, err := service.AuthenticateBroker(ctx, "whalerein-device-d1", "device:d1", "device-2"); !errors.Is(err, ErrUnauthorized) {
		t.Fatalf("device id mismatch = %v", err)
	}
	// Revoked device (no live grant) is rejected for MQTT.
	fs.grants[grantKey("h1", "d2")] = false
	if _, err := service.AuthenticateBroker(ctx, "whalerein-device-d2", "device:d2", "device-2"); !errors.Is(err, ErrUnauthorized) {
		t.Fatalf("revoked device = %v", err)
	}
	fs.activeErr = errors.New("db down")
	if _, err := service.AuthenticateBroker(ctx, "whalerein-device-d1", "device:d1", "device-token"); err == nil {
		t.Fatal("expected store error")
	}
	fs.activeErr = nil
	if _, err := service.AuthenticateBroker(ctx, "x", "other", "x"); !errors.Is(err, ErrUnauthorized) {
		t.Fatalf("unknown = %v", err)
	}
	if !service.AuthenticateAccessCredentials("access", "access-pass") || service.AuthenticateAccessCredentials("access", "x") {
		t.Fatal("access credentials check")
	}
}

func TestProbeRequiresProvisioningCredentialWithoutRegisteringHostOrGrantingTopics(t *testing.T) {
	service, fs, _, _ := newService(t)
	ctx := context.Background()
	clientID := "whalerein-probe-0123456789abcdef0123456789abcdef"
	fs.ensureErr = errors.New("must not provision hosts")
	identity, err := service.AuthenticateBroker(ctx, clientID, "probe:new-host", "host-token")
	if err != nil || identity.Kind != KindProbe || identity.HostID != "new-host" {
		t.Fatalf("probe identity = %+v, error = %v", identity, err)
	}
	if len(fs.hosts) != 0 {
		t.Fatal("probe registered a host")
	}
	if _, err := service.AuthenticateBroker(ctx, clientID, "probe:new-host", "wrong"); !errors.Is(err, ErrUnauthorized) {
		t.Fatalf("wrong probe credential = %v", err)
	}
	for _, action := range []ACLAction{ActionPublish, ActionSubscribe} {
		for _, topic := range []string{"whalerein/v1/h/new-host/request", "whalerein/v1/h/new-host/result", "whalerein/v1/h/new-host/event", "$SYS/#", "#"} {
			if allowed, err := service.AuthorizeACL(ctx, identity, action, topic); err != nil || allowed {
				t.Fatalf("probe allowed %s on %s", action, topic)
			}
		}
	}
	for _, pair := range [][2]string{
		{"whalerein-host-new-host", "probe:new-host"},
		{clientID, "host:new-host"},
		{"whalerein-probe-short", "probe:new-host"},
		{clientID, "probe:"}, {clientID, "probe:bad/#"},
	} {
		if _, err := service.IdentityFromCredentials(pair[0], pair[1]); err == nil {
			t.Fatal("malformed probe identity accepted")
		}
	}
}

func TestAuthorizeACLAllowsOnlyPrincipalOwnedTopics(t *testing.T) {
	service, fs, _, _ := newService(t)
	ctx := context.Background()
	fs.grants[grantKey("h1", "d1")] = true
	access := BrokerIdentity{Kind: KindAccess}
	host := BrokerIdentity{Kind: KindHost, HostID: "h1"}
	device := BrokerIdentity{Kind: KindDevice, DeviceID: "d1"}

	cases := []struct {
		name     string
		identity BrokerIdentity
		action   ACLAction
		topic    string
		allow    bool
	}{
		{"allows Access to subscribe to host results", access, ActionSubscribe, "whalerein/v1/h/+/result", true},
		{"allows Access to subscribe to host events", access, ActionSubscribe, "whalerein/v1/h/+/event", true},
		{"allows Access to subscribe to broker connect events", access, ActionSubscribe, "$SYS/brokers/+/clients/+/connected", true},
		{"allows Access to subscribe to broker disconnect events", access, ActionSubscribe, "$SYS/brokers/+/clients/+/disconnected", true},
		{"allows Access to subscribe to device last wills", access, ActionSubscribe, "whalerein/v1/presence/device/+", true},
		{"denies Access subscription to host requests", access, ActionSubscribe, "whalerein/v1/h/h1/request", false},
		{"allows Access to publish host requests", access, ActionPublish, "whalerein/v1/h/h1/request", true},
		{"allows Access to publish device events", access, ActionPublish, "whalerein/v1/h/h1/d/d1/event", true},
		{"allows Access to publish host-scoped presence", access, ActionPublish, "whalerein/v1/h/h1/presence", true},
		{"denies Access publication to host results", access, ActionPublish, "whalerein/v1/h/h1/result", false},
		{"denies Access publication to device will topics", access, ActionPublish, "whalerein/v1/presence/device/d1", false},
		{"denies Access publication to invalid topics", access, ActionPublish, "bad/topic", false},
		{"allows host subscription to its own requests", host, ActionSubscribe, "whalerein/v1/h/h1/request", true},
		{"denies host subscription to another host requests", host, ActionSubscribe, "whalerein/v1/h/h2/request", false},
		{"denies host wildcard subscriptions", host, ActionSubscribe, "whalerein/v1/h/+/request", false},
		{"allows host subscription to its own presence", host, ActionSubscribe, "whalerein/v1/h/h1/presence", true},
		{"denies host subscription to another host presence", host, ActionSubscribe, "whalerein/v1/h/h2/presence", false},
		{"denies host wildcard presence subscription", host, ActionSubscribe, "whalerein/v1/h/+/presence", false},
		{"denies host publication to presence", host, ActionPublish, "whalerein/v1/h/h1/presence", false},
		{"allows host publication to its own results", host, ActionPublish, "whalerein/v1/h/h1/result", true},
		{"allows host publication to its own events", host, ActionPublish, "whalerein/v1/h/h1/event", true},
		{"denies host publication to request topics", host, ActionPublish, "whalerein/v1/h/h1/request", false},
		{"denies host publication to device event topics", host, ActionPublish, "whalerein/v1/h/h1/d/d1/event", false},
		{"allows device subscription to its granted event topic", device, ActionSubscribe, "whalerein/v1/h/h1/d/d1/event", true},
		{"denies device subscription to another device events", device, ActionSubscribe, "whalerein/v1/h/h1/d/d2/event", false},
		{"denies device subscription to host event topic", device, ActionSubscribe, "whalerein/v1/h/h1/event", false},
		{"denies device wildcard subscriptions", device, ActionSubscribe, "whalerein/v1/h/h1/d/+/event", false},
		{"denies device subscription to last wills", device, ActionSubscribe, "whalerein/v1/presence/device/+", false},
		{"allows device publication to its own last will", device, ActionPublish, "whalerein/v1/presence/device/d1", true},
		{"denies device publication to another device last will", device, ActionPublish, "whalerein/v1/presence/device/d2", false},
		{"denies device publication to a will wildcard", device, ActionPublish, "whalerein/v1/presence/device/+", false},
		{"denies device publication to request topic", device, ActionPublish, "whalerein/v1/h/h1/request", false},
		{"denies unknown broker principal", BrokerIdentity{Kind: "other"}, ActionSubscribe, "whalerein/v1/h/h1/request", false},
		{"denies unsupported ACL action", host, ACLAction("admin"), "whalerein/v1/h/h1/request", false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			allowed, err := service.AuthorizeACL(ctx, tc.identity, tc.action, tc.topic)
			if err != nil {
				t.Fatalf("err = %v", err)
			}
			if allowed != tc.allow {
				t.Fatalf("allowed = %v, want %v", allowed, tc.allow)
			}
		})
	}
	fs.grants[grantKey("h1", "d1")] = false
	if allowed, _ := service.AuthorizeACL(ctx, device, ActionSubscribe, "whalerein/v1/h/h1/d/d1/event"); allowed {
		t.Fatal("revoked grant allowed")
	}
	fs.grants[grantKey("h1", "d1")] = true
	fs.grantErr = errors.New("db down")
	if _, err := service.AuthorizeACL(ctx, device, ActionSubscribe, "whalerein/v1/h/h1/d/d1/event"); err == nil {
		t.Fatal("expected store error")
	}
}

func successAck(req *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
	var notice whalereinv1.RevocationNotice
	if err := proto.Unmarshal(req.GetBody(), &notice); err != nil {
		return nil, err
	}
	ack, _ := proto.Marshal(&whalereinv1.RevocationAck{ReplayId: notice.GetReplayId(), DeviceIds: notice.GetDeviceIds()})
	return &whalereinv1.HostResult{Status: whalereinv1.OnlineStatus_succeeded, ResultBody: ack}, nil
}

func TestRevokeDeviceDeniesAccessAndReplaysHostNotices(t *testing.T) {
	service, fs, presence, transport := newService(t)
	ctx := context.Background()
	fs.devices["d1"] = store.Device{DeviceID: "d1"}
	fs.hosts["h1"] = store.Host{HostID: "h1"}
	fs.hosts["h2"] = store.Host{HostID: "h2"}
	fs.grants[grantKey("h1", "d1")] = true
	fs.grants[grantKey("h2", "d1")] = true
	presence.online["h1"] = true
	transport.awaitFn = successAck

	if err := service.RevokeDevice(ctx, "d1"); err != nil {
		t.Fatal(err)
	}
	if len(transport.disconnected) != 1 || transport.disconnected[0] != "d1" {
		t.Fatalf("disconnected = %v", transport.disconnected)
	}
	if len(fs.markCalls) != 1 || fs.markCalls[0] != "h1:d1" {
		t.Fatalf("mark calls = %v", fs.markCalls)
	}
	if len(transport.awaited) != 1 {
		t.Fatalf("awaited = %d", len(transport.awaited))
	}
	var notice whalereinv1.RevocationNotice
	if err := proto.Unmarshal(transport.awaited[0].GetBody(), &notice); err != nil {
		t.Fatal(err)
	}
	if notice.GetFinal() || len(notice.GetDeviceIds()) != 1 || notice.GetDeviceIds()[0] != "d1" {
		t.Fatalf("notice = %+v", &notice)
	}

	if err := service.RevokeDevice(ctx, "missing"); !errors.Is(err, ErrNotFound) {
		t.Fatalf("missing = %v", err)
	}
	if err := service.RevokeDevice(ctx, ""); !errors.Is(err, ErrBadRequest) {
		t.Fatalf("empty = %v", err)
	}

	// Transport failure leaves replayed_at unset.
	fs.devices["d2"] = store.Device{DeviceID: "d2"}
	fs.grants[grantKey("h1", "d2")] = true
	transport.awaitErr = errors.New("timeout")
	if err := service.RevokeDevice(ctx, "d2"); err != nil {
		t.Fatalf("transport error should not fail the API: %v", err)
	}
	transport.awaitErr = nil

	// Invalid acknowledgements do not mark replay.
	for name, fn := range map[string]func(*whalereinv1.HostRequest) (*whalereinv1.HostResult, error){
		"does not acknowledge revocation when host returns nil result": func(*whalereinv1.HostRequest) (*whalereinv1.HostResult, error) { return nil, nil },
		"rejects revocation when host result is failed": func(*whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
			return &whalereinv1.HostResult{Status: whalereinv1.OnlineStatus_failed}, nil
		},
		"rejects revocation with malformed acknowledgement": func(*whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
			return &whalereinv1.HostResult{Status: whalereinv1.OnlineStatus_succeeded, ResultBody: []byte("x")}, nil
		},
		"rejects acknowledgement with a different replay ID": func(req *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
			ack, _ := proto.Marshal(&whalereinv1.RevocationAck{ReplayId: "other", DeviceIds: []string{"d3"}})
			return &whalereinv1.HostResult{Status: whalereinv1.OnlineStatus_succeeded, ResultBody: ack}, nil
		},
		"rejects acknowledgement omitting the revoked device": func(req *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
			var notice whalereinv1.RevocationNotice
			_ = proto.Unmarshal(req.GetBody(), &notice)
			ack, _ := proto.Marshal(&whalereinv1.RevocationAck{ReplayId: notice.GetReplayId()})
			return &whalereinv1.HostResult{Status: whalereinv1.OnlineStatus_succeeded, ResultBody: ack}, nil
		},
	} {
		t.Run(name, func(t *testing.T) {
			fs.devices["d3"] = store.Device{DeviceID: "d3"}
			fs.grants[grantKey("h1", "d3")] = true
			fs.markCalls = nil
			transport.awaitFn = fn
			if err := service.RevokeDevice(ctx, "d3"); err != nil {
				t.Fatal(err)
			}
			if len(fs.markCalls) != 0 {
				t.Fatalf("marked after invalid ack: %v", fs.markCalls)
			}
		})
	}

	transport.awaitFn = successAck
	fs.devices["d4"] = store.Device{DeviceID: "d4"}
	fs.grants[grantKey("h1", "d4")] = true
	transport.disconnectErr = errors.New("kick failed")
	if err := service.RevokeDevice(ctx, "d4"); err != nil {
		t.Fatalf("disconnect error should be non-fatal: %v", err)
	}
	transport.disconnectErr = nil

	fs.devices["d5"] = store.Device{DeviceID: "d5"}
	fs.grants[grantKey("h1", "d5")] = true
	fs.markErr = errors.New("db down")
	if err := service.RevokeDevice(ctx, "d5"); err == nil {
		t.Fatal("expected mark error")
	}
	fs.markErr = nil
	fs.revokeErr = errors.New("db down")
	if err := service.RevokeDevice(ctx, "d1"); err == nil {
		t.Fatal("expected revoke error")
	}
}

func TestReplayPendingRevocationsWaitsForAcknowledgement(t *testing.T) {
	service, fs, _, transport := newService(t)
	ctx := context.Background()
	now := time.Now()
	fs.pending = []store.Revocation{{DeviceID: "d1", RevokedAt: now.Add(-time.Minute)}, {DeviceID: "d2", RevokedAt: now}}
	transport.awaitFn = successAck
	if err := service.ReplayPendingRevocations(ctx, "h1"); err != nil {
		t.Fatal(err)
	}
	if len(transport.awaited) != 2 || len(transport.published) != 0 {
		t.Fatalf("awaited=%d published=%d", len(transport.awaited), len(transport.published))
	}
	var pendingNotice, final whalereinv1.RevocationNotice
	if err := proto.Unmarshal(transport.awaited[0].GetBody(), &pendingNotice); err != nil {
		t.Fatal(err)
	}
	if final := pendingNotice.GetFinal(); final {
		t.Fatal("pending batch marked final")
	}
	if err := proto.Unmarshal(transport.awaited[1].GetBody(), &final); err != nil {
		t.Fatal(err)
	}
	if !final.GetFinal() || len(final.GetDeviceIds()) != 0 {
		t.Fatalf("final = %+v", &final)
	}
	if len(fs.markCalls) != 1 || fs.markCalls[0] != "h1:d1,d2" {
		t.Fatalf("mark calls = %v", fs.markCalls)
	}

	// No pending rows still publishes the final empty batch and awaits its ack.
	transport.awaited = nil
	transport.published = nil
	if err := service.ReplayPendingRevocations(ctx, "h2"); err != nil {
		t.Fatal(err)
	}
	if len(transport.awaited) != 1 || len(transport.published) != 0 {
		t.Fatalf("empty replay awaited=%d published=%d", len(transport.awaited), len(transport.published))
	}

	// A failed pending ack must not publish final.
	fs.pending = []store.Revocation{{DeviceID: "d3", RevokedAt: now}}
	transport.awaited = nil
	transport.published = nil
	transport.awaitErr = errors.New("timeout")
	if err := service.ReplayPendingRevocations(ctx, "h1"); err == nil {
		t.Fatal("expected ack error")
	}
	if len(transport.awaited) != 1 {
		t.Fatalf("final should not be attempted: %d", len(transport.awaited))
	}
	transport.awaitErr = nil
	transport.awaitFn = func(req *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
		ack, _ := proto.Marshal(&whalereinv1.RevocationAck{ReplayId: "wrong"})
		return &whalereinv1.HostResult{Status: whalereinv1.OnlineStatus_succeeded, ResultBody: ack}, nil
	}
	if err := service.ReplayPendingRevocations(ctx, "h1"); err == nil {
		t.Fatal("expected ack validation error")
	}
	transport.awaitFn = successAck
	fs.markErr = errors.New("db down")
	if err := service.ReplayPendingRevocations(ctx, "h1"); err == nil {
		t.Fatal("expected mark error")
	}
	fs.markErr = nil
	transport.awaitErr = errors.New("mqtt down")
	fs.pending = nil
	transport.awaited = nil
	if err := service.ReplayPendingRevocations(ctx, "h1"); err == nil {
		t.Fatal("expected final await error")
	}
	transport.awaitErr = nil
	fs.pendingErr = errors.New("db down")
	if err := service.ReplayPendingRevocations(ctx, "h1"); err == nil {
		t.Fatal("expected pending error")
	}
	if err := service.ReplayPendingRevocations(ctx, ""); !errors.Is(err, ErrBadRequest) {
		t.Fatalf("empty host = %v", err)
	}
}

func TestOpaqueIDAndHostRevocationRequestRejectInvalidSignatures(t *testing.T) {
	if opaqueID() == opaqueID() || len(opaqueID()) < 20 {
		t.Fatal("opaque id")
	}
	notice := &whalereinv1.RevocationNotice{ReplayId: "r", DeviceIds: []string{"d"}}
	req, err := revocationRequest("h1", notice, 1, time.Unix(10, 0))
	if err != nil || req.GetHostId() != "h1" || req.GetKind() != whalereinv1.RequestKind_REQUEST_KIND_REVOCATION {
		t.Fatalf("req = %+v err=%v", req, err)
	}
	if err := verifyRevocationAck(nil, notice); err == nil {
		t.Fatal("nil ack")
	}
}

func TestRevocationAuditIncludesOriginEvenWhenHostOffline(t *testing.T) {
	service, fs, _, _ := newService(t)
	var output bytes.Buffer
	service.logger = slog.New(slog.NewJSONHandler(&output, nil))
	fs.devices["device-audit-raw"] = store.Device{DeviceID: "device-audit-raw"}
	fs.grants[grantKey("h1", "device-audit-raw")] = true
	ctx := diagnostics.WithTraceID(context.Background(), "operation-audit")
	if err := service.RevokeDevice(ctx, "device-audit-raw"); err != nil {
		t.Fatal(err)
	}
	var record map[string]any
	if err := json.Unmarshal(bytes.TrimSpace(output.Bytes()), &record); err != nil {
		t.Fatal(err)
	}
	if record["source"] != "device_request" || record["trace_id"] != "operation-audit" || record["msg"] != "revocation committed" || record["revoked_at_unix_ms"] == nil {
		t.Fatal("missing revocation provenance")
	}
	if strings.Contains(output.String(), "device-audit-raw") {
		t.Fatal("raw device leaked")
	}
	output.Reset()
	fs.grants[grantKey("h1", "device-audit-raw")] = true
	if err := service.RevokeHostGrant(ctx, "h1", "device-audit-raw"); err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(output.String(), `"source":"host_settings"`) {
		t.Fatal("missing host origin")
	}
}

func TestDeviceCanOnlyPublishItsOwnTransportReceipts(t *testing.T) {
	service, fs, _, _ := newService(t)
	ctx := context.Background()
	fs.grants[grantKey("h1", "d1")] = true
	identity := BrokerIdentity{Kind: KindDevice, DeviceID: "d1"}
	for _, item := range []struct {
		action  ACLAction
		topic   string
		allowed bool
	}{
		{ActionPublish, "whalerein/v1/h/h1/d/d1/ack", true},
		{ActionSubscribe, "whalerein/v1/h/h1/d/d1/ack", false},
		{ActionPublish, "whalerein/v1/h/h1/d/other/ack", false},
		{ActionPublish, "whalerein/v1/h/h1/d/d1/event", false},
	} {
		allowed, err := service.AuthorizeACL(ctx, identity, item.action, item.topic)
		if err != nil || allowed != item.allowed {
			t.Fatal("invalid receipt authorization")
		}
	}
	fs.grantErr = errors.New("offline")
	if _, err := service.AuthorizeACL(ctx, identity, ActionPublish, "whalerein/v1/h/h1/d/d1/ack"); err == nil {
		t.Fatal("authorization lookup failure ignored")
	}
}

type repairDuringDisconnect struct {
	*fakeTransport
	repair func()
}

func (r *repairDuringDisconnect) DisconnectDevice(context.Context, string) error {
	r.repair()
	return nil
}

func TestRevocationAfterConcurrentRepairDoesNotSendAnOldGrantNotice(t *testing.T) {
	service, fs, presence, transport := newService(t)
	fs.devices["d"] = store.Device{DeviceID: "d"}
	fs.hosts["h"] = store.Host{HostID: "h"}
	fs.grants[grantKey("h", "d")] = true
	presence.online["h"] = true
	service.transport = &repairDuringDisconnect{fakeTransport: transport, repair: func() {
		fs.grants[grantKey("h", "d")] = true
		fs.pending = nil
	}}
	if err := service.RevokeDevice(context.Background(), "d"); err != nil {
		t.Fatal(err)
	}
	if len(transport.awaited) != 0 {
		t.Fatal("stale notice sent after repair")
	}
	if !fs.grants[grantKey("h", "d")] {
		t.Fatal("repair grant removed")
	}
}

func TestReplayCarriesEachGrantGenerationAndUsesItForAcknowledgement(t *testing.T) {
	service, fs, _, transport := newService(t)
	fs.pending = []store.Revocation{{DeviceID: "a", GrantGeneration: 2, RevokedAt: time.Unix(10, 0)}, {DeviceID: "b", GrantGeneration: 7, RevokedAt: time.Unix(20, 0)}}
	transport.awaitFn = func(request *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
		var notice whalereinv1.RevocationNotice
		if err := proto.Unmarshal(request.Body, &notice); err != nil {
			t.Fatal(err)
		}
		if !notice.Final && (notice.GrantGenerations["a"] != 2 || notice.GrantGenerations["b"] != 7) {
			t.Fatal("grant incarnation lost in replay batch")
		}
		return successAck(request)
	}
	if err := service.ReplayPendingRevocations(context.Background(), "h"); err != nil {
		t.Fatal(err)
	}
}
