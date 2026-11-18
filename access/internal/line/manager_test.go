package line

import (
	"context"
	"errors"
	"fmt"
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
			request.fields = append(request.fields, "id "+value)
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
		id := ""
		for _, field := range request.fields {
			if strings.HasPrefix(field, "id ") {
				id = strings.TrimPrefix(field, "id ")
			}
		}
		extra, err := respond(request)
		if err != nil {
			return err
		}
		reply := "whalerein/1\nid " + id + "\nok\n" + extra
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

func TestRevokeGrantDeniesAndKicksTheScopedConnection(t *testing.T) {
	grant := store.AdmissionGrant{HostID: "h1", DeviceID: "d1", GrantGeneration: 2, RevocationGeneration: 3, DeviceRevocationGeneration: 4}
	fs := &fakeStore{grantFor: map[string]store.AdmissionGrant{"h1|d1": grant}}
	control, requests := newTestControl(t, func(recordedRequest) (string, error) { return "", nil })
	manager := newTestManager(t, fs, control, &fakeApplier{}, testPresence(t))
	if err := manager.RevokeGrant(context.Background(), "h1", "d1"); err != nil {
		t.Fatal(err)
	}
	if len(*requests) != 1 || (*requests)[0].op != "deny" {
		t.Fatalf("requests = %+v", *requests)
	}
	want := []string{"device d1", fmt.Sprintf("generation %d", policy.Revision(grant)), "host h1", "kick 1"}
	got := (*requests)[0].fields[1:] // drop the synthetic id field
	if strings.Join(got, "|") != strings.Join(want, "|") {
		t.Fatalf("deny fields = %v, want %v", got, want)
	}
	if err := manager.RevokeGrant(context.Background(), "h2", "d1"); err == nil {
		t.Fatal("missing grant ignored")
	}
}

func TestRevokeDeviceDeniesEveryHostScopeThenKicksTheDevice(t *testing.T) {
	fs := &fakeStore{
		deviceGrants: map[string][]store.AdmissionGrant{"d1": {
			{HostID: "h1", DeviceID: "d1"},
			{HostID: "h2", DeviceID: "d1"},
		}},
		grantFor: map[string]store.AdmissionGrant{
			"h1|d1": {HostID: "h1", DeviceID: "d1", GrantGeneration: 5},
			"h2|d1": {HostID: "h2", DeviceID: "d1", GrantGeneration: 6},
		},
	}
	control, requests := newTestControl(t, func(recordedRequest) (string, error) { return "", nil })
	manager := newTestManager(t, fs, control, &fakeApplier{}, testPresence(t))
	if err := manager.RevokeDevice(context.Background(), "d1"); err != nil {
		t.Fatal(err)
	}
	if len(*requests) != 3 {
		t.Fatalf("requests = %+v", *requests)
	}
	deniedHosts := map[string]bool{}
	for _, request := range (*requests)[:2] {
		if request.op != "deny" {
			t.Fatalf("expected deny, got %+v", request)
		}
		joined := strings.Join(request.fields, " ")
		if !strings.Contains(joined, "kick 0") || !strings.Contains(joined, "device d1") {
			t.Fatalf("deny fields = %v", request.fields)
		}
		for _, host := range []string{"h1", "h2"} {
			if strings.Contains(joined, "host "+host) {
				deniedHosts[host] = true
			}
		}
	}
	last := (*requests)[2]
	if last.op != "kick" || strings.Join(last.fields[1:], "|") != "username device:d1" {
		t.Fatalf("final kick = %+v", last)
	}
	if !deniedHosts["h1"] || !deniedHosts["h2"] || len(deniedHosts) != 2 {
		t.Fatalf("denied hosts = %v", deniedHosts)
	}

	fs.deviceErr = errors.New("db down")
	if err := manager.RevokeDevice(context.Background(), "d1"); err == nil {
		t.Fatal("grant listing failure ignored")
	}
}

func TestRevokeDeviceSurfacesGrantAndKickFailures(t *testing.T) {
	fs := &fakeStore{
		deviceGrants: map[string][]store.AdmissionGrant{"d1": {{HostID: "h1", DeviceID: "d1"}}},
		grantFor:     map[string]store.AdmissionGrant{"h1|d1": {HostID: "h1", DeviceID: "d1"}},
	}
	snapshot := func(recordedRequest) (string, error) { return "", nil }
	control, _ := newTestControl(t, snapshot)
	fs.grantErr = errors.New("grant down")
	if err := newTestManager(t, fs, control, &fakeApplier{}, testPresence(t)).RevokeDevice(context.Background(), "d1"); err == nil {
		t.Fatal("grant read failure ignored")
	}
	fs.grantErr = nil

	denyFailure, _ := newTestControl(t, func(request recordedRequest) (string, error) {
		if request.op == "deny" {
			return "", errors.New("deny failed")
		}
		return "", nil
	})
	if err := newTestManager(t, fs, denyFailure, &fakeApplier{}, testPresence(t)).RevokeDevice(context.Background(), "d1"); err == nil {
		t.Fatal("deny failure ignored")
	}

	kickFailure, _ := newTestControl(t, func(request recordedRequest) (string, error) {
		if request.op == "kick" {
			return "", errors.New("kick failed")
		}
		return "", nil
	})
	if err := newTestManager(t, fs, kickFailure, &fakeApplier{}, testPresence(t)).RevokeDevice(context.Background(), "d1"); err == nil {
		t.Fatal("kick failure ignored")
	}
}

func TestHeartbeatRefreshesThePolicyWindowAndReportsFailure(t *testing.T) {
	control, requests := newTestControl(t, func(recordedRequest) (string, error) { return "", nil })
	manager := newTestManager(t, &fakeStore{}, control, &fakeApplier{}, testPresence(t))
	if err := manager.Heartbeat(context.Background()); err != nil {
		t.Fatal(err)
	}
	if len(*requests) != 1 || (*requests)[0].op != "ping" || strings.Join((*requests)[0].fields[1:], "|") != "ttl 10000" {
		t.Fatalf("heartbeat = %+v", *requests)
	}
	failing, _ := newTestControl(t, func(request recordedRequest) (string, error) {
		return "", errors.New("ping down")
	})
	if err := newTestManager(t, &fakeStore{}, failing, &fakeApplier{}, testPresence(t)).Heartbeat(context.Background()); err == nil {
		t.Fatal("heartbeat failure ignored")
	}
}

func TestIsNotConnectedRecognizesBrokerFailures(t *testing.T) {
	for _, tc := range []struct {
		err  error
		want bool
	}{
		{nil, false},
		{errors.New("not_found"), true},
		{errors.New("client not connected"), true},
		{errors.New("denied"), false},
	} {
		if got := isNotConnected(tc.err); got != tc.want {
			t.Fatalf("isNotConnected(%v) = %v", tc.err, got)
		}
	}
}

func TestRecordPolicySkipsAnUnchangedProjection(t *testing.T) {
	fs := &fakeStore{deviceGrants: map[string][]store.AdmissionGrant{
		"d1": {{HostID: "h1", DeviceID: "d1", Active: true, GrantGeneration: 4}},
	}}
	control, requests := newTestControl(t, func(recordedRequest) (string, error) { return "", nil })
	manager := newTestManager(t, fs, control, &fakeApplier{}, testPresence(t))

	manager.RecordPolicy("", 9)
	if manager.currentlyApplied("", 9) {
		t.Fatal("empty device recorded")
	}
	manager.RecordPolicy("d1", policy.Revision(fs.deviceGrants["d1"][0]))
	manager.applyDevicePolicy("d1")
	if len(*requests) != 0 {
		t.Fatalf("unchanged policy projected: %+v", *requests)
	}
}

func TestApplyDevicePolicyHandlesEveryOutcome(t *testing.T) {
	grant := store.AdmissionGrant{HostID: "h1", DeviceID: "d1", Active: true, GrantGeneration: 4}
	fs := &fakeStore{deviceGrants: map[string][]store.AdmissionGrant{"d1": {grant}}}

	// Listing failure logs and increments the reconcile error counter.
	fs.deviceErr = errors.New("db down")
	control, _ := newTestControl(t, func(recordedRequest) (string, error) { return "", nil })
	manager := newTestManager(t, fs, control, &fakeApplier{}, testPresence(t))
	manager.applyDevicePolicy("d1")
	fs.deviceErr = nil

	// A rejected projection is absorbed; pairing already succeeded.
	denied, _ := newTestControl(t, func(request recordedRequest) (string, error) {
		if request.op == "set" {
			return "", errors.New("denied")
		}
		return "", nil
	})
	newTestManager(t, fs, denied, &fakeApplier{}, testPresence(t)).applyDevicePolicy("d1")

	// A device that vanished between commit and projection is tolerated.
	notFound, _ := newTestControl(t, func(request recordedRequest) (string, error) {
		if request.op == "set" {
			return "", errors.New("broker control set: not_found")
		}
		return "", nil
	})
	newTestManager(t, fs, notFound, &fakeApplier{}, testPresence(t)).applyDevicePolicy("d1")

	// Success records the revision so the next pass can skip it.
	okControl, requests := newTestControl(t, func(recordedRequest) (string, error) { return "", nil })
	succeeded := newTestManager(t, fs, okControl, &fakeApplier{}, testPresence(t))
	succeeded.applyDevicePolicy("d1")
	if !succeeded.currentlyApplied("d1", policy.Revision(grant)) {
		t.Fatal("successful projection was not recorded")
	}
	if len(*requests) != 1 {
		t.Fatalf("requests = %+v", *requests)
	}
}

func TestRevokeGrantSurfacesGrantReadFailure(t *testing.T) {
	fs := &fakeStore{grantErr: errors.New("db down")}
	control, requests := newTestControl(t, func(recordedRequest) (string, error) { return "", nil })
	if err := newTestManager(t, fs, control, &fakeApplier{}, testPresence(t)).RevokeGrant(context.Background(), "h1", "d1"); err == nil {
		t.Fatal("grant read failure ignored")
	}
	if len(*requests) != 0 {
		t.Fatal("deny sent without a durable revision")
	}
}

func TestRevokeHostDeniesCurrentGrantIncarnationsThenClosesTheHost(t *testing.T) {
	fs := &fakeStore{grantFor: map[string]store.AdmissionGrant{}}
	control, requests := newTestControl(t, func(recordedRequest) (string, error) { return "", nil })
	manager := newTestManager(t, fs, control, &fakeApplier{}, testPresence(t))
	if err := manager.RevokeHost(context.Background(), "h1", map[string]uint64{"d2": 9, "d1": 4}); err != nil {
		t.Fatal(err)
	}
	if len(*requests) != 3 {
		t.Fatalf("requests = %+v", *requests)
	}
	// Device iteration is sorted so a retry issues the same operations.
	if (*requests)[0].op != "deny" || !strings.Contains(strings.Join((*requests)[0].fields, " "), "device d1") ||
		!strings.Contains(strings.Join((*requests)[0].fields, " "), "generation 4") {
		t.Fatalf("first deny = %+v", (*requests)[0])
	}
	if (*requests)[1].op != "deny" || !strings.Contains(strings.Join((*requests)[1].fields, " "), "device d2") ||
		!strings.Contains(strings.Join((*requests)[1].fields, " "), "generation 9") {
		t.Fatalf("second deny = %+v", (*requests)[1])
	}
	if (*requests)[2].op != "kick" || !strings.Contains(strings.Join((*requests)[2].fields, " "), "username host:h1") {
		t.Fatalf("host kick = %+v", (*requests)[2])
	}
}

func TestRevokeHostSwallowsAnAlreadyClosedSessionAndSurfacesOtherFailures(t *testing.T) {
	control, _ := newTestControl(t, func(request recordedRequest) (string, error) {
		if request.op == "kick" {
			return "", errors.New("client not connected")
		}
		return "", nil
	})
	manager := newTestManager(t, &fakeStore{grantFor: map[string]store.AdmissionGrant{}}, control, &fakeApplier{}, testPresence(t))
	if err := manager.RevokeHost(context.Background(), "h1", nil); err != nil {
		t.Fatalf("an already closed Host session is not an error: %v", err)
	}

	failing, _ := newTestControl(t, func(request recordedRequest) (string, error) {
		return "", errors.New("broker unavailable")
	})
	manager = newTestManager(t, &fakeStore{grantFor: map[string]store.AdmissionGrant{}}, failing, &fakeApplier{}, testPresence(t))
	if err := manager.RevokeHost(context.Background(), "h1", map[string]uint64{"d1": 1}); err == nil {
		t.Fatal("a line failure must be surfaced")
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
