package line

import (
	"context"
	"errors"
	"io"
	"log/slog"
	"strings"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/broker"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/policy"
	"github.com/whalerein/whalerein/access/internal/store"
)

type fakeStore struct {
	grants       []store.AdmissionGrant
	deviceGrants map[string][]store.AdmissionGrant
	grantFor     map[string]store.AdmissionGrant
	listErr      error
	deviceErr    error
	grantErr     error
}

func (f *fakeStore) ListAdmissionGrants(context.Context) ([]store.AdmissionGrant, error) {
	if f.listErr != nil {
		return nil, f.listErr
	}
	return f.grants, nil
}

func (f *fakeStore) ListDeviceGrants(_ context.Context, deviceID string) ([]store.AdmissionGrant, error) {
	if f.deviceErr != nil {
		return nil, f.deviceErr
	}
	return f.deviceGrants[deviceID], nil
}

func (f *fakeStore) AdmissionGrantFor(_ context.Context, hostID, deviceID string) (store.AdmissionGrant, error) {
	if f.grantErr != nil {
		return store.AdmissionGrant{}, f.grantErr
	}
	grant, ok := f.grantFor[hostID+"|"+deviceID]
	if !ok {
		return store.AdmissionGrant{}, store.ErrNotFound
	}
	return grant, nil
}

type fakeApplier struct {
	err   error
	calls []string
}

func (f *fakeApplier) Allow(_ context.Context, hostID, deviceID string) error {
	f.calls = append(f.calls, hostID+"|"+deviceID)
	return f.err
}

type recordedRequest struct {
	id     string
	op     string
	fields []string
}

func parseRequest(t *testing.T, payload []byte) recordedRequest {
	t.Helper()
	lines := strings.Split(strings.TrimSpace(string(payload)), "\n")
	if len(lines) < 3 || lines[0] != "whalerein/1" {
		t.Fatalf("control payload = %q", payload)
	}
	request := recordedRequest{}
	for _, line := range lines[1:] {
		key, value, ok := strings.Cut(line, " ")
		if !ok {
			request.fields = append(request.fields, line)
			continue
		}
		switch key {
		case "id":
			request.id = value
		case "op":
			request.op = value
		default:
			request.fields = append(request.fields, line)
		}
	}
	return request
}

// newTestControl builds a real broker.Control whose publish function records
// each request and synthesizes the reply through the public reply topic.
func newTestControl(t *testing.T, respond func(request recordedRequest) (extra string, err error)) (*broker.Control, *[]recordedRequest) {
	t.Helper()
	var control *broker.Control
	requests := &[]recordedRequest{}
	publish := func(_ context.Context, topic string, payload []byte) error {
		if topic != broker.ControlRequestTopic {
			t.Fatalf("control topic = %q", topic)
		}
		request := parseRequest(t, payload)
		*requests = append(*requests, request)
		extra, err := respond(request)
		if err != nil {
			return err
		}
		if request.op == "snapshot" && !strings.Contains(extra, "epoch ") {
			extra = "epoch test-broker-epoch\n" + extra
		}
		reply := "whalerein/1\nid " + request.id + "\nok\n" + extra
		control.HandleMessage(broker.ControlReplyFilter, []byte(reply))
		return nil
	}
	var err error
	control, err = broker.NewControl(publish)
	if err != nil {
		t.Fatal(err)
	}
	return control, requests
}

func testPresence(t *testing.T) *broker.Presence {
	t.Helper()
	return broker.NewPresence(time.Minute, diagnostics.NewMetrics(), slog.New(slog.NewTextHandler(io.Discard, nil)))
}

func newTestManager(t *testing.T, store Store, control *broker.Control, applier Applier, presence *broker.Presence) *Manager {
	t.Helper()
	manager, err := New(Dependencies{
		Store: store, Control: control, Admission: applier, Presence: presence,
		Config:  Config{PolicyTTL: 10 * time.Second, Timeout: time.Second},
		Metrics: diagnostics.NewMetrics(),
		Masker:  diagnostics.NewMasker(),
		Logger:  slog.New(slog.NewTextHandler(io.Discard, nil)),
	})
	if err != nil {
		t.Fatal(err)
	}
	return manager
}

func TestNewRequiresDependenciesAndAppliesDefaults(t *testing.T) {
	if _, err := New(Dependencies{}); err == nil {
		t.Fatal("expected missing dependency error")
	}
	control, _ := newTestControl(t, func(recordedRequest) (string, error) { return "", nil })
	manager, err := New(Dependencies{Store: &fakeStore{}, Control: control, Admission: &fakeApplier{}, Presence: testPresence(t)})
	if err != nil {
		t.Fatal(err)
	}
	if manager.cfg.PolicyTTL != 10*time.Second || manager.cfg.Timeout != 5*time.Second {
		t.Fatalf("defaults = %+v", manager.cfg)
	}
}

func TestReconcileAppliesSnapshotAndProjectsActiveDevicePolicy(t *testing.T) {
	presence := testPresence(t)
	fs := &fakeStore{
		grants: []store.AdmissionGrant{
			{HostID: "h1", DeviceID: "d1", Active: true, GrantGeneration: 1},
			{HostID: "h2", DeviceID: "d2", Active: false, GrantGeneration: 1},
		},
	}
	control, requests := newTestControl(t, func(request recordedRequest) (string, error) {
		if request.op == "snapshot" {
			return "seq 50\nclient whalerein-host-h1 host h1 - 1 0 connected 50 1000\n" +
				"client whalerein-device-d1 device - d1 2 1 connected 51 1001\n", nil
		}
		return "", nil
	})
	manager := newTestManager(t, fs, control, &fakeApplier{}, presence)
	if err := manager.Reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	if !presence.Available() || !presence.IsHostOnline(context.Background(), "h1") {
		t.Fatal("snapshot was not applied to presence")
	}
	if len(*requests) != 3 {
		t.Fatalf("requests = %+v", *requests)
	}
	if (*requests)[0].op != "snapshot" {
		t.Fatalf("first request = %+v", (*requests)[0])
	}
	set := (*requests)[1]
	if set.op != "set" {
		t.Fatalf("set request = %+v", set)
	}
	joined := strings.Join(set.fields, "\n")
	for _, want := range []string{
		"client whalerein-device-d1", "kind device", "device d1", "revision 1", "ttl 10000",
		"acl read 1 h1 whalerein/v1/h/h1/d/d1/event",
	} {
		if !strings.Contains(joined, want) {
			t.Fatalf("set request missing %q: %s", want, joined)
		}
	}
	ping := (*requests)[2]
	if ping.op != "ping" || !strings.Contains(strings.Join(ping.fields, "\n"), "revision 1") {
		t.Fatalf("ping = %+v", ping)
	}
}

func TestReconcileProjectsOnlyConnectedDevicesAndFailsClosedElsewhere(t *testing.T) {
	snapshotWithDevice := func(request recordedRequest) (string, error) {
		if request.op == "snapshot" {
			return "seq 7\nclient whalerein-device-d1 device - d1 4 1 connected 7 1000\n", nil
		}
		return "", nil
	}
	fs := &fakeStore{grants: []store.AdmissionGrant{{HostID: "h1", DeviceID: "d1", Active: true, GrantGeneration: 1}}}

	// No connected device: the active grant needs no live policy projection.
	control, requests := newTestControl(t, func(recordedRequest) (string, error) { return "", nil })
	manager := newTestManager(t, fs, control, &fakeApplier{}, testPresence(t))
	if err := manager.Reconcile(context.Background()); err != nil {
		t.Fatalf("idle reconcile: %v", err)
	}
	for _, request := range *requests {
		if request.op == "set" {
			t.Fatalf("projected policy without a connection: %+v", request)
		}
	}

	// A connected device whose grant disappeared is cleared and kicked.
	staleControl, staleRequests := newTestControl(t, func(request recordedRequest) (string, error) {
		if request.op == "snapshot" {
			return "seq 8\nclient whalerein-device-d2 device - d2 5 1 connected 8 1000\n", nil
		}
		return "", nil
	})
	if err := newTestManager(t, fs, staleControl, &fakeApplier{}, testPresence(t)).Reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	removed := false
	for _, request := range *staleRequests {
		if request.op == "remove" && strings.Contains(strings.Join(request.fields, " "), "whalerein-device-d2") &&
			strings.Contains(strings.Join(request.fields, " "), "kick 1") {
			removed = true
		}
	}
	if !removed {
		t.Fatalf("stale connected device was not removed: %+v", *staleRequests)
	}

	// A device that disappears between snapshot and set is tolerated.
	notFound, _ := newTestControl(t, func(request recordedRequest) (string, error) {
		if request.op == "snapshot" {
			return snapshotWithDevice(request)
		}
		if request.op == "set" {
			return "", errors.New("broker control set: not_found")
		}
		return "", nil
	})
	if err := newTestManager(t, fs, notFound, &fakeApplier{}, testPresence(t)).Reconcile(context.Background()); err != nil {
		t.Fatalf("not-connected set should be tolerated: %v", err)
	}

	// Repeating reconcile at the same revision must not re-send the policy.
	cachedControl, cachedRequests := newTestControl(t, snapshotWithDevice)
	cached := newTestManager(t, fs, cachedControl, &fakeApplier{}, testPresence(t))
	if err := cached.Reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	before := len(*cachedRequests)
	if err := cached.Reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	sets := 0
	for _, request := range (*cachedRequests)[before:] {
		if request.op == "set" {
			sets++
		}
	}
	if sets != 0 {
		t.Fatalf("reconcile re-sent %d unchanged policies", sets)
	}

	snapshotFailure, _ := newTestControl(t, func(recordedRequest) (string, error) {
		return "", errors.New("snapshot down")
	})
	if err := newTestManager(t, fs, snapshotFailure, &fakeApplier{}, testPresence(t)).Reconcile(context.Background()); err == nil {
		t.Fatal("snapshot failure ignored")
	}

	fs.listErr = errors.New("db down")
	okControl, _ := newTestControl(t, snapshotWithDevice)
	if err := newTestManager(t, fs, okControl, &fakeApplier{}, testPresence(t)).Reconcile(context.Background()); err == nil {
		t.Fatal("grant listing failure ignored")
	}
	fs.listErr = nil

	setFailure, _ := newTestControl(t, func(request recordedRequest) (string, error) {
		if request.op == "snapshot" {
			return snapshotWithDevice(request)
		}
		if request.op == "set" {
			return "", errors.New("denied")
		}
		return "", nil
	})
	if err := newTestManager(t, fs, setFailure, &fakeApplier{}, testPresence(t)).Reconcile(context.Background()); err == nil {
		t.Fatal("set failure ignored")
	}

	pingFailure, _ := newTestControl(t, func(request recordedRequest) (string, error) {
		if request.op == "ping" {
			return "", errors.New("ping down")
		}
		return snapshotWithDevice(request)
	})
	if err := newTestManager(t, fs, pingFailure, &fakeApplier{}, testPresence(t)).Reconcile(context.Background()); err == nil {
		t.Fatal("ping failure ignored")
	}
}

func TestApplyPairingAllowsAdmissionAndProjectsPolicy(t *testing.T) {
	applier := &fakeApplier{}
	fs := &fakeStore{deviceGrants: map[string][]store.AdmissionGrant{
		"d1": {{HostID: "h1", DeviceID: "d1", Active: true, GrantGeneration: 3}},
	}}
	control, requests := newTestControl(t, func(recordedRequest) (string, error) { return "", nil })
	manager := newTestManager(t, fs, control, applier, testPresence(t))
	if err := manager.ApplyPairing(context.Background(), "h1", "d1"); err != nil {
		t.Fatal(err)
	}
	if len(applier.calls) != 1 || applier.calls[0] != "h1|d1" {
		t.Fatalf("admission calls = %v", applier.calls)
	}
	deadline := time.Now().Add(2 * time.Second)
	for len(*requests) == 0 && time.Now().Before(deadline) {
		time.Sleep(5 * time.Millisecond)
	}
	if len(*requests) != 1 || (*requests)[0].op != "set" {
		t.Fatalf("requests = %+v", *requests)
	}
	joined := strings.Join((*requests)[0].fields, "\n")
	if !strings.Contains(joined, "revision 3") || !strings.Contains(joined, "acl read 3 h1 whalerein/v1/h/h1/d/d1/event") {
		t.Fatalf("pairing policy = %s", joined)
	}
}

func TestApplyPairingToleratesNotConnectedAndSurfacesFailures(t *testing.T) {
	fs := &fakeStore{deviceGrants: map[string][]store.AdmissionGrant{
		"d1": {{HostID: "h1", DeviceID: "d1", Active: true, GrantGeneration: 1}},
	}}
	notConnected, _ := newTestControl(t, func(request recordedRequest) (string, error) {
		if request.op == "set" {
			return "", errors.New("client not connected")
		}
		return "", nil
	})
	if err := newTestManager(t, fs, notConnected, &fakeApplier{}, testPresence(t)).ApplyPairing(context.Background(), "h1", "d1"); err != nil {
		t.Fatalf("not-connected pairing should be tolerated: %v", err)
	}

	allowFailure := &fakeApplier{err: errors.New("admission down")}
	okControl, requests := newTestControl(t, func(recordedRequest) (string, error) { return "", nil })
	if err := newTestManager(t, fs, okControl, allowFailure, testPresence(t)).ApplyPairing(context.Background(), "h1", "d1"); err == nil {
		t.Fatal("admission failure ignored")
	}
	if len(*requests) != 0 {
		t.Fatal("policy was projected after admission failure")
	}

	// Broker projection failures are best effort: the device receives the
	// committed policy when it authenticates, and reconcile retries later.
	fs.deviceErr = errors.New("db down")
	if err := newTestManager(t, fs, okControl, &fakeApplier{}, testPresence(t)).ApplyPairing(context.Background(), "h1", "d1"); err != nil {
		t.Fatalf("background projection failure must not fail pairing: %v", err)
	}
	fs.deviceErr = nil

	setFailure, _ := newTestControl(t, func(request recordedRequest) (string, error) {
		if request.op == "set" {
			return "", errors.New("denied")
		}
		return "", nil
	})
	if err := newTestManager(t, fs, setFailure, &fakeApplier{}, testPresence(t)).ApplyPairing(context.Background(), "h1", "d1"); err != nil {
		t.Fatalf("background set failure must not fail pairing: %v", err)
	}
}

func scopedGrant(hostID, deviceID string, grantGeneration, revocationGeneration, deviceRevocationGeneration uint64, connections ...store.ScopeConnection) store.ScopeGrant {
	return store.ScopeGrant{
		HostID: hostID, DeviceID: deviceID,
		GrantGeneration: grantGeneration, RevocationGeneration: revocationGeneration,
		DeviceRevocationGeneration: deviceRevocationGeneration,
		Connections:                connections,
	}
}

func freezeDevice(t *testing.T, presence *broker.Presence, deviceID string, refs ...store.ScopeConnection) {
	t.Helper()
	clients := make([]broker.SnapshotClient, 0, len(refs))
	for _, ref := range refs {
		clients = append(clients, broker.SnapshotClient{
			Kind: "device", DeviceID: deviceID, ClientID: ref.ClientID,
			ConnID: ref.Ref.ID, Epoch: ref.Ref.Epoch, State: "connected",
		})
	}
	presence.SetBrokerEpoch(refs[0].Ref.Epoch)
	presence.ApplySnapshot(clients, 1)
}

func TestRevokeGrantFencesFrozenGenerationAndClosesOnlyItsConnection(t *testing.T) {
	connection := store.ScopeConnection{Ref: store.ConnectionRef{Epoch: "epoch-a", ID: 7}, ClientID: "whalerein-device-d1"}
	presence := testPresence(t)
	freezeDevice(t, presence, "d1", connection)
	control, requests := newTestControl(t, func(request recordedRequest) (string, error) {
		if request.op == "deny" {
			return "matched 1\n", nil
		}
		return "", nil
	})
	manager := newTestManager(t, &fakeStore{}, control, &fakeApplier{}, presence)
	scope := store.RevocationScope{HostID: "h1", DeviceID: "d1", Grants: []store.ScopeGrant{
		scopedGrant("h1", "d1", 2, 4, 0, connection),
	}}
	receipt, err := manager.RevokeGrant(context.Background(), scope)
	if err != nil {
		t.Fatal(err)
	}
	if len(*requests) != 2 {
		t.Fatalf("requests = %+v", *requests)
	}
	base := strings.Join((*requests)[0].fields, " ")
	if (*requests)[0].op != "deny" || !strings.Contains(base, "generation 4194306") ||
		!strings.Contains(base, "kick 0") || strings.Contains(base, "conn ") {
		t.Fatalf("base deny = %+v", (*requests)[0])
	}
	scoped := strings.Join((*requests)[1].fields, " ")
	if (*requests)[1].op != "deny" || !strings.Contains(scoped, "conn 7") || !strings.Contains(scoped, "kick 1") {
		t.Fatalf("scoped deny = %+v", (*requests)[1])
	}
	for _, request := range *requests {
		if request.op == "kick" || strings.Contains(strings.Join(request.fields, " "), "username ") {
			t.Fatalf("authorization revocation used an unfenced username kick: %+v", request)
		}
	}
	if len(receipt.Applied) != 1 || receipt.Applied[0] != connection {
		t.Fatalf("receipt = %+v", receipt)
	}
	if receipt.Primary() == nil || *receipt.Primary() != connection.Ref {
		t.Fatalf("primary rejection = %+v", receipt.Primary())
	}
}

func TestRevokeGrantTreatsAnAbsentOrReplacedConnectionAsEnforced(t *testing.T) {
	connection := store.ScopeConnection{Ref: store.ConnectionRef{Epoch: "epoch-a", ID: 7}, ClientID: "whalerein-device-d1"}
	control, requests := newTestControl(t, func(request recordedRequest) (string, error) {
		if request.op == "deny" {
			return "matched 0\n", nil
		}
		return "", nil
	})
	manager := newTestManager(t, &fakeStore{}, control, &fakeApplier{}, testPresence(t))
	scope := store.RevocationScope{HostID: "h1", DeviceID: "d1", Grants: []store.ScopeGrant{
		scopedGrant("h1", "d1", 2, 4, 0, connection),
	}}
	receipt, err := manager.RevokeGrant(context.Background(), scope)
	if err != nil {
		t.Fatal(err)
	}
	if !receipt.Absent || len(receipt.Applied) != 0 || receipt.Primary() != nil {
		t.Fatalf("absent scope receipt = %+v", receipt)
	}
	// The generation fence is still installed even when the target vanished.
	if len(*requests) == 0 || (*requests)[0].op != "deny" {
		t.Fatalf("requests = %+v", *requests)
	}
}

func TestScopedRevocationSkipsConnectionsFromAnotherBrokerEpoch(t *testing.T) {
	old := store.ScopeConnection{Ref: store.ConnectionRef{Epoch: "epoch-old", ID: 7}, ClientID: "whalerein-device-d1"}
	current := store.ScopeConnection{Ref: store.ConnectionRef{Epoch: "epoch-new", ID: 7}, ClientID: "whalerein-device-d1"}
	presence := testPresence(t)
	freezeDevice(t, presence, "d1", current)
	control, requests := newTestControl(t, func(request recordedRequest) (string, error) {
		if request.op == "deny" {
			return "matched 1\n", nil
		}
		return "", nil
	})
	manager := newTestManager(t, &fakeStore{}, control, &fakeApplier{}, presence)
	scope := store.RevocationScope{HostID: "h1", DeviceID: "d1", Grants: []store.ScopeGrant{
		scopedGrant("h1", "d1", 2, 4, 0, old),
	}}
	receipt, err := manager.RevokeGrant(context.Background(), scope)
	if err != nil {
		t.Fatal(err)
	}
	for _, request := range *requests {
		if strings.Contains(strings.Join(request.fields, " "), "conn 7") {
			t.Fatalf("an old-epoch connection id was targeted: %+v", request)
		}
	}
	if !receipt.Absent {
		t.Fatalf("an old-epoch scope must be absent: %+v", receipt)
	}
}

func TestQueuedDeliveryFenceIsInstalledForAnAbsentConnection(t *testing.T) {
	control, requests := newTestControl(t, func(request recordedRequest) (string, error) {
		if request.op == "deny" {
			return "matched 0\n", nil
		}
		return "", nil
	})
	manager := newTestManager(t, &fakeStore{}, control, &fakeApplier{}, testPresence(t))
	scope := store.RevocationScope{HostID: "h1", DeviceID: "d1", Grants: []store.ScopeGrant{
		scopedGrant("h1", "d1", 3, 2, 1),
	}}
	receipt, err := manager.RevokeGrant(context.Background(), scope)
	if err != nil {
		t.Fatal(err)
	}
	if len(*requests) != 1 || (*requests)[0].op != "deny" {
		t.Fatalf("requests = %+v", *requests)
	}
	fields := strings.Join((*requests)[0].fields, " ")
	if !strings.Contains(fields, "generation ") || !strings.Contains(fields, "kick 0") {
		t.Fatalf("queued delivery fence was not installed: %+v", (*requests)[0])
	}
	if !receipt.Absent {
		t.Fatalf("an absent connection must count as enforced: %+v", receipt)
	}
}

func TestSameIDReplacementWithNewerAuthorityIsNotClosed(t *testing.T) {
	// The frozen connection id now belongs to a replacement whose policy has a
	// newer generation; the Broker reports no matching stale entry.
	connection := store.ScopeConnection{Ref: store.ConnectionRef{Epoch: "epoch-a", ID: 7}, ClientID: "whalerein-device-d1"}
	presence := testPresence(t)
	freezeDevice(t, presence, "d1", connection)
	control, requests := newTestControl(t, func(request recordedRequest) (string, error) {
		if request.op == "deny" {
			return "matched 0\n", nil
		}
		return "", nil
	})
	manager := newTestManager(t, &fakeStore{}, control, &fakeApplier{}, presence)
	receipt, err := manager.RevokeGrant(context.Background(), store.RevocationScope{
		HostID: "h1", DeviceID: "d1", Grants: []store.ScopeGrant{scopedGrant("h1", "d1", 2, 4, 0, connection)},
	})
	if err != nil {
		t.Fatal(err)
	}
	if !receipt.Absent {
		t.Fatalf("the replacement was reported as closed: %+v", receipt)
	}
	for _, request := range *requests {
		if request.op == "kick" || strings.Contains(strings.Join(request.fields, " "), "username ") {
			t.Fatalf("the replacement was closed by an unfenced kick: %+v", request)
		}
	}
}

func TestScopedRevocationIsIsolatedToTheFrozenHostAndDevice(t *testing.T) {
	control, requests := newTestControl(t, func(request recordedRequest) (string, error) {
		if request.op == "deny" {
			return "matched 0\n", nil
		}
		return "", nil
	})
	manager := newTestManager(t, &fakeStore{}, control, &fakeApplier{}, testPresence(t))
	scope := store.RevocationScope{HostID: "h1", DeviceID: "d1", Grants: []store.ScopeGrant{
		scopedGrant("h1", "d1", 3, 0, 0),
	}}
	if _, err := manager.RevokeGrant(context.Background(), scope); err != nil {
		t.Fatal(err)
	}
	for _, request := range *requests {
		fields := strings.Join(request.fields, " ")
		if !strings.Contains(fields, "device d1") || !strings.Contains(fields, "host h1") {
			t.Fatalf("scope leaked outside host/device: %+v", request)
		}
		if strings.Contains(fields, "h2") || strings.Contains(fields, "d2") || strings.Contains(fields, "username ") {
			t.Fatalf("revocation escaped its frozen scope: %+v", request)
		}
	}
}

func TestRevokeDeviceFencesEveryHostScopeThenClosesOnlyFrozenConnections(t *testing.T) {
	connection := store.ScopeConnection{Ref: store.ConnectionRef{Epoch: "epoch-a", ID: 8}, ClientID: "whalerein-device-d1-send"}
	presence := testPresence(t)
	freezeDevice(t, presence, "d1", connection)
	control, requests := newTestControl(t, func(request recordedRequest) (string, error) {
		if request.op == "deny" {
			return "matched 1\n", nil
		}
		return "", nil
	})
	manager := newTestManager(t, &fakeStore{}, control, &fakeApplier{}, presence)
	scope := store.RevocationScope{DeviceID: "d1", DeviceWide: true, Grants: []store.ScopeGrant{
		scopedGrant("h1", "d1", 5, 0, 0, connection),
		scopedGrant("h2", "d1", 6, 0, 0, connection),
	}}
	receipt, err := manager.RevokeDevice(context.Background(), scope)
	if err != nil {
		t.Fatal(err)
	}
	deniedHosts := map[string]bool{}
	scopedKicks := 0
	for _, request := range *requests {
		joined := strings.Join(request.fields, " ")
		if request.op != "deny" {
			t.Fatalf("unexpected operation = %+v", request)
		}
		if strings.Contains(joined, "username ") {
			t.Fatalf("device revocation used a username kick: %+v", request)
		}
		for _, hostID := range []string{"h1", "h2"} {
			if strings.Contains(joined, "host "+hostID) {
				deniedHosts[hostID] = true
			}
		}
		if strings.Contains(joined, "conn 8") && strings.Contains(joined, "kick 1") {
			scopedKicks++
		}
	}
	if !deniedHosts["h1"] || !deniedHosts["h2"] {
		t.Fatalf("denied hosts = %v", deniedHosts)
	}
	if scopedKicks != 2 {
		t.Fatalf("scoped connection denies = %d, want 2", scopedKicks)
	}
	if len(receipt.Applied) != 1 {
		t.Fatalf("receipt connections were duplicated: %+v", receipt.Applied)
	}
}

func TestRevokeDeviceSurfacesBrokerDenyFailures(t *testing.T) {
	denyFailure, _ := newTestControl(t, func(request recordedRequest) (string, error) {
		if request.op == "deny" {
			return "", errors.New("deny failed")
		}
		return "", nil
	})
	scope := store.RevocationScope{DeviceID: "d1", DeviceWide: true, Grants: []store.ScopeGrant{
		scopedGrant("h1", "d1", 1, 0, 0),
	}}
	if _, err := newTestManager(t, &fakeStore{}, denyFailure, &fakeApplier{}, testPresence(t)).RevokeDevice(context.Background(), scope); err == nil {
		t.Fatal("deny failure ignored")
	}
}

func TestScopedEnforcementFailureKeepsTheOperationRetryable(t *testing.T) {
	connection := store.ScopeConnection{Ref: store.ConnectionRef{Epoch: "epoch-a", ID: 7}, ClientID: "whalerein-device-d1"}
	presence := testPresence(t)
	freezeDevice(t, presence, "d1", connection)
	scope := store.RevocationScope{HostID: "h1", DeviceID: "d1", Grants: []store.ScopeGrant{
		scopedGrant("h1", "d1", 2, 4, 0, connection),
	}}
	failing, requests := newTestControl(t, func(request recordedRequest) (string, error) {
		if request.op == "deny" {
			return "", errors.New("broker control timeout")
		}
		return "", nil
	})
	manager := newTestManager(t, &fakeStore{}, failing, &fakeApplier{}, presence)
	if _, err := manager.RevokeGrant(context.Background(), scope); err == nil {
		t.Fatal("an unconfirmed Broker fence must keep the durable operation pending")
	}
	if len(*requests) != 1 {
		t.Fatalf("requests = %+v", *requests)
	}

	// The retry re-issues exactly the frozen generation and connection.
	ok, retried := newTestControl(t, func(request recordedRequest) (string, error) {
		if request.op == "deny" {
			return "matched 1\n", nil
		}
		return "", nil
	})
	manager = newTestManager(t, &fakeStore{}, ok, &fakeApplier{}, presence)
	if _, err := manager.RevokeGrant(context.Background(), scope); err != nil {
		t.Fatal(err)
	}
	if len(*retried) != 2 || strings.Join((*retried)[0].fields, " ") != strings.Join((*requests)[0].fields, " ") {
		t.Fatalf("retry did not replay the frozen scope: %+v vs %+v", *retried, *requests)
	}
}

func TestReconnectNewGenerationTargetsOnlyTheFrozenConnection(t *testing.T) {
	old := store.ScopeConnection{Ref: store.ConnectionRef{Epoch: "epoch-a", ID: 7}, ClientID: "whalerein-device-d1"}
	replacement := store.ScopeConnection{Ref: store.ConnectionRef{Epoch: "epoch-a", ID: 8}, ClientID: "whalerein-device-d1"}
	presence := testPresence(t)
	freezeDevice(t, presence, "d1", replacement)
	control, requests := newTestControl(t, func(request recordedRequest) (string, error) {
		if request.op == "deny" {
			return "matched 0\n", nil
		}
		return "", nil
	})
	manager := newTestManager(t, &fakeStore{}, control, &fakeApplier{}, presence)
	receipt, err := manager.RevokeGrant(context.Background(), store.RevocationScope{
		HostID: "h1", DeviceID: "d1", Grants: []store.ScopeGrant{scopedGrant("h1", "d1", 2, 4, 0, old)},
	})
	if err != nil {
		t.Fatal(err)
	}
	for _, request := range *requests {
		fields := strings.Join(request.fields, " ")
		if strings.Contains(fields, "conn 8") {
			t.Fatalf("the replacement connection was targeted: %+v", request)
		}
	}
	if !receipt.Absent {
		t.Fatalf("the replacement was reported as applied: %+v", receipt)
	}
}

func TestScopedRevocationHandlesIncompleteGrantsAndConnectionFailures(t *testing.T) {
	first := store.ScopeConnection{Ref: store.ConnectionRef{Epoch: "epoch-a", ID: 7}, ClientID: "whalerein-device-d1"}
	second := store.ScopeConnection{Ref: store.ConnectionRef{Epoch: "epoch-a", ID: 6}, ClientID: "whalerein-device-d1"}
	third := store.ScopeConnection{Ref: store.ConnectionRef{Epoch: "epoch-a", ID: 8}, ClientID: "whalerein-device-d1-extra"}
	control, requests := newTestControl(t, func(request recordedRequest) (string, error) {
		fields := strings.Join(request.fields, " ")
		if request.op == "deny" && strings.Contains(fields, "conn ") {
			return "", errors.New("client not connected")
		}
		if request.op == "deny" {
			return "matched 1\n", nil
		}
		return "", nil
	})
	manager := newTestManager(t, &fakeStore{}, control, &fakeApplier{}, testPresence(t))
	scope := store.RevocationScope{HostID: "h1", DeviceID: "d1", Grants: []store.ScopeGrant{
		{HostID: "h1", DeviceID: "d1", GrantGeneration: 1, Connections: []store.ScopeConnection{first, second, third}},
		{HostID: "", DeviceID: "d1", GrantGeneration: 2},
		{HostID: "h2", GrantGeneration: 3},
	}}
	receipt, err := manager.RevokeGrant(context.Background(), scope)
	if err != nil {
		t.Fatal(err)
	}
	if len(*requests) != 5 {
		t.Fatalf("requests = %+v", *requests)
	}
	for _, request := range *requests {
		if strings.Contains(strings.Join(request.fields, " "), "generation 2") {
			t.Fatalf("a grant without a Host was enforced: %+v", request)
		}
	}
	// The final grant has no device id and falls back to the scope device id.
	last := strings.Join((*requests)[4].fields, " ")
	if !strings.Contains(last, "generation 3") || !strings.Contains(last, "device d1") {
		t.Fatalf("scope device fallback = %s", last)
	}
	if !receipt.Absent || len(receipt.Applied) != 0 {
		t.Fatalf("unreachable connections must count as absent: %+v", receipt)
	}

	// A hard connection-deny failure keeps the operation pending.
	failing, _ := newTestControl(t, func(request recordedRequest) (string, error) {
		if request.op == "deny" && strings.Contains(strings.Join(request.fields, " "), "conn ") {
			return "", errors.New("broker denied")
		}
		return "matched 1\n", nil
	})
	manager = newTestManager(t, &fakeStore{}, failing, &fakeApplier{}, testPresence(t))
	if _, err := manager.RevokeGrant(context.Background(), store.RevocationScope{
		HostID: "h1", DeviceID: "d1",
		Grants: []store.ScopeGrant{scopedGrant("h1", "d1", 1, 0, 0, first)},
	}); err == nil {
		t.Fatal("a hard connection-deny failure must be surfaced")
	}
}

func TestRevokeHostRemoveFailureIsSurfaced(t *testing.T) {
	hostConnection := store.ScopeConnection{Ref: store.ConnectionRef{Epoch: "epoch-a", ID: 11}, ClientID: "whalerein-host-h1"}
	control, _ := newTestControl(t, func(request recordedRequest) (string, error) {
		if request.op == "remove" {
			return "", errors.New("broker failure")
		}
		return "", nil
	})
	manager := newTestManager(t, &fakeStore{}, control, &fakeApplier{}, testPresence(t))
	if _, err := manager.RevokeHost(context.Background(), store.RevocationScope{HostID: "h1", HostConnection: &hostConnection}); err == nil {
		t.Fatal("a Host remove failure must be surfaced so enforcement stays pending")
	}
}

func TestScopedConnectionsAreDeduplicatedAndRequireATargetClient(t *testing.T) {
	valid := store.ScopeConnection{Ref: store.ConnectionRef{Epoch: "epoch-a", ID: 7}, ClientID: "whalerein-device-d1"}
	invalid := store.ScopeConnection{Ref: store.ConnectionRef{Epoch: "epoch-a", ID: 8}}
	control, requests := newTestControl(t, func(request recordedRequest) (string, error) {
		if request.op == "deny" {
			return "matched 1\n", nil
		}
		return "", nil
	})
	manager := newTestManager(t, &fakeStore{}, control, &fakeApplier{}, testPresence(t))
	scope := store.RevocationScope{HostID: "h1", DeviceID: "d1", Grants: []store.ScopeGrant{
		scopedGrant("h1", "d1", 2, 4, 0, valid, valid, invalid),
	}}
	receipt, err := manager.RevokeGrant(context.Background(), scope)
	if err != nil {
		t.Fatal(err)
	}
	connDenies := 0
	for _, request := range *requests {
		if strings.Contains(strings.Join(request.fields, " "), "conn ") {
			connDenies++
		}
	}
	if connDenies != 1 {
		t.Fatalf("connection denies = %d, want 1: %+v", connDenies, *requests)
	}
	if len(receipt.Applied) != 1 || receipt.Applied[0] != valid {
		t.Fatalf("applied = %+v", receipt.Applied)
	}
}

func TestRevokeHostDeniesFrozenIncarnationsThenRemovesTheFrozenHostConnection(t *testing.T) {
	hostConnection := store.ScopeConnection{Ref: store.ConnectionRef{Epoch: "epoch-a", ID: 11}, ClientID: "whalerein-host-h1"}
	control, requests := newTestControl(t, func(request recordedRequest) (string, error) {
		switch request.op {
		case "deny":
			return "matched 1\n", nil
		case "remove":
			return "conn 11\n", nil
		}
		return "", nil
	})
	manager := newTestManager(t, &fakeStore{}, control, &fakeApplier{}, testPresence(t))
	scope := store.RevocationScope{HostID: "h1", HostConnection: &hostConnection, Grants: []store.ScopeGrant{
		scopedGrant("h1", "d2", 9, 0, 0),
		scopedGrant("h1", "d1", 4, 0, 0),
	}}
	receipt, err := manager.RevokeHost(context.Background(), scope)
	if err != nil {
		t.Fatal(err)
	}
	// Device iteration is sorted so a retry issues the same operations.
	if len(*requests) != 3 {
		t.Fatalf("requests = %+v", *requests)
	}
	if (*requests)[0].op != "deny" || !strings.Contains(strings.Join((*requests)[0].fields, " "), "device d1") ||
		!strings.Contains(strings.Join((*requests)[0].fields, " "), "generation 4") {
		t.Fatalf("first deny = %+v", (*requests)[0])
	}
	if (*requests)[1].op != "deny" || !strings.Contains(strings.Join((*requests)[1].fields, " "), "device d2") ||
		!strings.Contains(strings.Join((*requests)[1].fields, " "), "generation 9") {
		t.Fatalf("second deny = %+v", (*requests)[1])
	}
	remove := (*requests)[2]
	if remove.op != "remove" || !strings.Contains(strings.Join(remove.fields, " "), "client whalerein-host-h1") ||
		!strings.Contains(strings.Join(remove.fields, " "), "conn 11") ||
		strings.Contains(strings.Join(remove.fields, " "), "username ") {
		t.Fatalf("host remove = %+v", remove)
	}
	if len(receipt.Applied) != 1 || receipt.Applied[0] != hostConnection {
		t.Fatalf("receipt = %+v", receipt)
	}
}

func TestRevokeHostTreatsAnAbsentHostConnectionAsEnforced(t *testing.T) {
	hostConnection := store.ScopeConnection{Ref: store.ConnectionRef{Epoch: "epoch-a", ID: 11}, ClientID: "whalerein-host-h1"}
	control, _ := newTestControl(t, func(request recordedRequest) (string, error) {
		if request.op == "remove" {
			return "absent 1\n", nil
		}
		return "", nil
	})
	manager := newTestManager(t, &fakeStore{}, control, &fakeApplier{}, testPresence(t))
	receipt, err := manager.RevokeHost(context.Background(), store.RevocationScope{HostID: "h1", HostConnection: &hostConnection})
	if err != nil {
		t.Fatalf("an already closed Host session is not an error: %v", err)
	}
	if !receipt.Absent {
		t.Fatalf("absent host receipt = %+v", receipt)
	}
	if _, err := manager.RevokeHost(context.Background(), store.RevocationScope{HostID: "h1"}); err != nil {
		t.Fatalf("an offline Host with no frozen connection is enforced: %v", err)
	}
}

func TestScopedRevocationSwallowsAnUnreachableBrokerForAbsentScopes(t *testing.T) {
	failing, _ := newTestControl(t, func(recordedRequest) (string, error) {
		return "", errors.New("client not connected")
	})
	scope := store.RevocationScope{HostID: "h1", DeviceID: "d1", Grants: []store.ScopeGrant{
		scopedGrant("h1", "d1", 1, 0, 0),
	}}
	if _, err := newTestManager(t, &fakeStore{}, failing, &fakeApplier{}, testPresence(t)).RevokeGrant(context.Background(), scope); err != nil {
		t.Fatalf("an unreachable absent target should count as enforced: %v", err)
	}
}

// A managed broker heartbeat verifies capability, renews a live session once
// and removes a different session without replacing its ordinary Device ACLs.
func TestManagedHeartbeatRefreshesOnlyCurrentSessionPolicies(t *testing.T) {
	var capability bool
	var projectionError error
	control, recorded := newTestControl(t, func(request recordedRequest) (string, error) {
		if request.op != "snapshot" {
			return "", nil
		}
		if !capability {
			return "seq 50\n", nil
		}
		return "seq 50\nsession_capability 1\n" +
			"client whalerein-session-old device - d1 1 1 connected 50 1000 old\n" +
			"client whalerein-session-new device - d1 2 1 connected 50 1000 new\n" +
			"client whalerein-session-new-watch device - d1 3 2 connected 50 1000 new\n", nil
	})
	manager := newTestManager(t, &fakeStore{}, control, &fakeApplier{}, testPresence(t))
	manager.SetSessionPolicy(func(_ context.Context, id, deviceID string) (policy.Connection, error) {
		if deviceID != "d1" {
			t.Fatal("wrong Device identity")
		}
		if projectionError != nil || id == "old" {
			return policy.Connection{}, projectionError
		}
		return policy.Connection{Kind: policy.KindDevice, DeviceID: deviceID,
			SessionID: id, SessionRevision: 3, SessionExpiresAt: 2000, Revision: 2}, nil
	})
	ctx := context.Background()
	if err := manager.Heartbeat(ctx); err == nil || len(*recorded) != 1 {
		t.Fatalf("missing capability should be fail-closed: %+v %v", *recorded, err)
	}
	capability = true
	projectionError = errors.New("no current regional state")
	if err := manager.Heartbeat(ctx); !errors.Is(err, projectionError) || len(*recorded) != 2 {
		t.Fatalf("failed projection must not refresh the policy: %+v %v", *recorded, err)
	}
	projectionError = nil
	if err := manager.Heartbeat(ctx); err != nil {
		t.Fatal(err)
	}
	got := (*recorded)[2:]
	want := []string{"snapshot", "session_absent", "renew_session", "set", "ping"}
	if len(got) != len(want) {
		t.Fatalf("operations = %+v", got)
	}
	for i, op := range want {
		if got[i].op != op {
			t.Fatalf("operation %d = %s, want %s", i, got[i].op, op)
		}
	}
	if !strings.Contains(strings.Join(got[1].fields, "\n"), "session old") ||
		!strings.Contains(strings.Join(got[2].fields, "\n"), "session new\nsession_revision 3\ndeadline_ms 2000") ||
		!strings.Contains(strings.Join(got[3].fields, "\n"), "client whalerein-session-new\n") {
		t.Fatalf("unexpected policy scope: %+v", got)
	}
}
