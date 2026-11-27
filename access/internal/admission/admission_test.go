package admission

import (
	"context"
	"errors"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/prometheus/client_golang/prometheus/testutil"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/store"
)

type fakeStore struct {
	mu      sync.Mutex
	grants  map[Key]store.AdmissionGrant
	loads   map[Key]int
	total   int
	block   map[Key]chan struct{}
	entered map[Key]chan struct{}
	err     error
	listErr error
}

func newFakeStore() *fakeStore {
	return &fakeStore{
		grants:  map[Key]store.AdmissionGrant{},
		loads:   map[Key]int{},
		block:   map[Key]chan struct{}{},
		entered: map[Key]chan struct{}{},
	}
}

func (f *fakeStore) put(hostID, deviceID string, active bool, generation uint64) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.grants[Key{HostID: hostID, DeviceID: deviceID}] = store.AdmissionGrant{
		HostID: hostID, DeviceID: deviceID, Active: active, GrantGeneration: generation,
	}
}

func (f *fakeStore) blockKey(hostID, deviceID string) chan struct{} {
	f.mu.Lock()
	defer f.mu.Unlock()
	release := make(chan struct{})
	f.block[Key{HostID: hostID, DeviceID: deviceID}] = release
	return release
}

// blockKeySignal returns a channel closed after the grant value has been
// captured but before the read returns, plus the release channel. It makes the
// pause deterministic instead of sleep-based.
func (f *fakeStore) blockKeySignal(hostID, deviceID string) (<-chan struct{}, chan struct{}) {
	f.mu.Lock()
	defer f.mu.Unlock()
	key := Key{HostID: hostID, DeviceID: deviceID}
	release := make(chan struct{})
	entered := make(chan struct{})
	f.block[key] = release
	f.entered[key] = entered
	return entered, release
}

func (f *fakeStore) AdmissionGrantFor(ctx context.Context, hostID, deviceID string) (store.AdmissionGrant, error) {
	key := Key{HostID: hostID, DeviceID: deviceID}
	f.mu.Lock()
	f.loads[key]++
	f.total++
	release := f.block[key]
	err := f.err
	grant, ok := f.grants[key]
	if entered, blocked := f.entered[key]; blocked {
		close(entered)
		delete(f.entered, key)
	}
	f.mu.Unlock()
	if release != nil {
		select {
		case <-release:
		case <-ctx.Done():
			return store.AdmissionGrant{}, ctx.Err()
		}
	}
	if err != nil {
		return store.AdmissionGrant{}, err
	}
	if !ok {
		return store.AdmissionGrant{}, store.ErrNotFound
	}
	return grant, nil
}

func (f *fakeStore) ListAdmissionGrants(ctx context.Context) ([]store.AdmissionGrant, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.listErr != nil {
		return nil, f.listErr
	}
	items := make([]store.AdmissionGrant, 0, len(f.grants))
	for _, grant := range f.grants {
		items = append(items, grant)
	}
	return items, nil
}

func (f *fakeStore) loadCount(hostID, deviceID string) int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.loads[Key{HostID: hostID, DeviceID: deviceID}]
}

func (f *fakeStore) totalLoads() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.total
}

func newService(t *testing.T, source Store, cfg Config) *Service {
	t.Helper()
	service, err := New(Dependencies{
		Store:   source,
		Config:  cfg,
		Metrics: diagnostics.NewMetrics(),
	})
	if err != nil {
		t.Fatalf("new admission: %v", err)
	}
	return service
}

func TestAuthorizeServesWarmGrantWithoutDatabaseReads(t *testing.T) {
	source := newFakeStore()
	source.put("host", "device", true, 3)
	service := newService(t, source, Config{})

	if allowed, err := service.Authorize(context.Background(), "host", "device"); err != nil || !allowed {
		t.Fatalf("first authorize: allowed=%v err=%v", allowed, err)
	}
	loads := source.totalLoads()
	for i := 0; i < 100; i++ {
		allowed, err := service.Authorize(context.Background(), "host", "device")
		if err != nil || !allowed {
			t.Fatalf("warm authorize %d: allowed=%v err=%v", i, allowed, err)
		}
	}
	if got := source.totalLoads(); got != loads {
		t.Fatalf("warm decisions performed %d extra loads", got-loads)
	}
	if hits := testutil.ToFloat64(service.metrics.AdmissionHits); hits < 100 {
		t.Fatalf("admission hits = %v", hits)
	}
}

func TestConcurrentColdLoadsCoalescePerKey(t *testing.T) {
	source := newFakeStore()
	source.put("host", "device", true, 1)
	release := source.blockKey("host", "device")
	service := newService(t, source, Config{LoadTimeout: 2 * time.Second})

	var wg sync.WaitGroup
	results := make(chan bool, 32)
	for i := 0; i < 32; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			allowed, err := service.Authorize(context.Background(), "host", "device")
			if err != nil {
				t.Errorf("authorize: %v", err)
				return
			}
			results <- allowed
		}()
	}
	time.Sleep(100 * time.Millisecond)
	close(release)
	wg.Wait()
	close(results)
	for allowed := range results {
		if !allowed {
			t.Fatal("coalesced load denied an active grant")
		}
	}
	if got := source.loadCount("host", "device"); got != 1 {
		t.Fatalf("grant loads = %d, want 1", got)
	}
}

func TestAbsentAndFailingLoadsFailClosed(t *testing.T) {
	source := newFakeStore()
	service := newService(t, source, Config{})
	if allowed, err := service.Authorize(context.Background(), "host", "missing"); err != nil || allowed {
		t.Fatalf("absent grant: allowed=%v err=%v", allowed, err)
	}

	source2 := newFakeStore()
	source2.err = errors.New("database unavailable")
	service2 := newService(t, source2, Config{})
	if allowed, err := service2.Authorize(context.Background(), "host", "device"); err == nil || allowed {
		t.Fatalf("failing lookup: allowed=%v err=%v", allowed, err)
	}
}

func TestDenyFencesInFlightLoad(t *testing.T) {
	source := newFakeStore()
	source.put("host", "device", true, 2)
	release := source.blockKey("host", "device")
	service := newService(t, source, Config{LoadTimeout: 2 * time.Second})

	result := make(chan bool, 1)
	go func() {
		allowed, err := service.Authorize(context.Background(), "host", "device")
		if err != nil {
			t.Errorf("authorize: %v", err)
			return
		}
		result <- allowed
	}()
	time.Sleep(50 * time.Millisecond)
	service.Deny("host", "device")
	close(release)
	if allowed := <-result; allowed {
		t.Fatal("in-flight load reopened a denied grant")
	}
	if allowed, err := service.Authorize(context.Background(), "host", "device"); err != nil || allowed {
		t.Fatalf("denied grant authorized: allowed=%v err=%v", allowed, err)
	}
}

func TestAllowClearsDenyOnlyForAnActiveGrant(t *testing.T) {
	source := newFakeStore()
	source.put("host", "device", false, 2)
	service := newService(t, source, Config{})
	service.Deny("host", "device")
	if err := service.Allow(context.Background(), "host", "device"); err == nil {
		t.Fatal("allow accepted a revoked grant")
	}
	if !service.Denied("host", "device") {
		t.Fatal("failed allow cleared the denial")
	}

	source.put("host", "device", true, 3)
	if err := service.Allow(context.Background(), "host", "device"); err != nil {
		t.Fatalf("allow active grant: %v", err)
	}
	if allowed, err := service.Authorize(context.Background(), "host", "device"); err != nil || !allowed {
		t.Fatalf("repaired grant authorized: allowed=%v err=%v", allowed, err)
	}
}

func TestDeviceDenyCoversEveryHostUntilNarrowed(t *testing.T) {
	source := newFakeStore()
	source.put("host-a", "device", true, 1)
	source.put("host-b", "device", true, 1)
	service := newService(t, source, Config{})

	service.DenyDevice("device")
	if allowed, _ := service.Authorize(context.Background(), "host-a", "device"); allowed {
		t.Fatal("device-wide denial did not cover host-a")
	}
	if allowed, _ := service.Authorize(context.Background(), "host-b", "device"); allowed {
		t.Fatal("device-wide denial did not cover host-b")
	}
	service.Deny("host-a", "device")
	service.CompleteDeviceRevoke("device")
	if allowed, _ := service.Authorize(context.Background(), "host-a", "device"); allowed {
		t.Fatal("confirmed per-grant denial did not apply")
	}
	if allowed, _ := service.Authorize(context.Background(), "host-b", "device"); !allowed {
		t.Fatal("unrelated grant outside the revoked scope was denied")
	}
}

func TestReconcileDoesNotClearALocalDenialOrDeviceFence(t *testing.T) {
	source := newFakeStore()
	source.put("host", "device", true, 4)
	source.put("other", "device", false, 2)
	service := newService(t, source, Config{})
	service.Deny("host", "device")
	service.DenyDevice("device")

	if err := service.Reconcile(context.Background()); err != nil {
		t.Fatalf("reconcile: %v", err)
	}
	if allowed, err := service.Authorize(context.Background(), "host", "device"); err != nil || allowed {
		t.Fatalf("local denial cleared by reconcile: allowed=%v err=%v", allowed, err)
	}
	if allowed, err := service.Authorize(context.Background(), "other", "device"); err != nil || allowed {
		t.Fatalf("revoked grant reopened: allowed=%v err=%v", allowed, err)
	}
	service.CompleteDeviceRevoke("device")
	if err := service.Allow(context.Background(), "host", "device"); err != nil {
		t.Fatalf("confirmed pairing: %v", err)
	}
	if allowed, err := service.Authorize(context.Background(), "host", "device"); err != nil || !allowed {
		t.Fatalf("confirmed active grant not allowed: allowed=%v err=%v", allowed, err)
	}
}

func TestReconcileFailureKeepsDenials(t *testing.T) {
	source := newFakeStore()
	source.put("host", "device", true, 4)
	source.listErr = errors.New("database unavailable")
	service := newService(t, source, Config{})
	service.Deny("host", "device")
	if err := service.Reconcile(context.Background()); err == nil {
		t.Fatal("reconcile reported success without durable state")
	}
	if allowed, _ := service.Authorize(context.Background(), "host", "device"); allowed {
		t.Fatal("failed reconcile reopened a denial")
	}
}

func TestCheckNeverBlocksAndLoadsInBackground(t *testing.T) {
	source := newFakeStore()
	source.put("host", "device", true, 1)
	release := source.blockKey("host", "device")
	service := newService(t, source, Config{LoadTimeout: 2 * time.Second})

	started := time.Now()
	allowed, known := service.Check("host", "device")
	if allowed || known {
		t.Fatalf("cold check: allowed=%v known=%v", allowed, known)
	}
	if elapsed := time.Since(started); elapsed > 50*time.Millisecond {
		t.Fatalf("cold check blocked for %s", elapsed)
	}
	close(release)
	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		if allowed, known := service.Check("host", "device"); known {
			if !allowed {
				t.Fatal("warmed grant denied")
			}
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatal("background load never completed")
}

func TestExpiredGrantReloads(t *testing.T) {
	source := newFakeStore()
	source.put("host", "device", true, 1)
	service := newService(t, source, Config{TTL: 40 * time.Millisecond})
	if allowed, err := service.Authorize(context.Background(), "host", "device"); err != nil || !allowed {
		t.Fatalf("first authorize: allowed=%v err=%v", allowed, err)
	}
	time.Sleep(80 * time.Millisecond)
	if allowed, err := service.Authorize(context.Background(), "host", "device"); err != nil || !allowed {
		t.Fatalf("second authorize: allowed=%v err=%v", allowed, err)
	}
	if got := source.loadCount("host", "device"); got < 2 {
		t.Fatalf("expired grant was not reloaded: loads=%d", got)
	}
}

func TestStalledLoadLeavesUnrelatedGrantsLive(t *testing.T) {
	source := newFakeStore()
	source.put("host-a", "device-a", true, 1)
	source.put("host-b", "device-b", true, 1)
	service := newService(t, source, Config{LoadTimeout: 2 * time.Second})
	if allowed, err := service.Authorize(context.Background(), "host-b", "device-b"); err != nil || !allowed {
		t.Fatalf("warm grant: allowed=%v err=%v", allowed, err)
	}
	release := source.blockKey("host-a", "device-a")
	go func() {
		_, _ = service.Authorize(context.Background(), "host-a", "device-a")
	}()
	time.Sleep(30 * time.Millisecond)
	started := time.Now()
	allowed, known := service.Check("host-b", "device-b")
	if !allowed || !known {
		t.Fatalf("unrelated grant: allowed=%v known=%v", allowed, known)
	}
	if elapsed := time.Since(started); elapsed > 20*time.Millisecond {
		t.Fatalf("unrelated grant waited %s for a stalled load", elapsed)
	}
	close(release)
}

func TestReconcileBoundsCacheEntries(t *testing.T) {
	source := newFakeStore()
	service := newService(t, source, Config{MaximumEntries: 2})
	for i := 0; i < 6; i++ {
		source.put("host", "device-"+strings.Repeat("x", i+1), true, 1)
	}
	if err := service.Reconcile(context.Background()); err != nil {
		t.Fatalf("reconcile: %v", err)
	}
	if got := service.cache.GetMaximum(); got != 2 {
		t.Fatalf("cache maximum = %d, want 2", got)
	}
}

func TestWarmReconcilesAndCountsCachedEntries(t *testing.T) {
	source := newFakeStore()
	source.put("host", "device", true, 1)
	source.put("host", "revoked", false, 1)
	service := newService(t, source, Config{})
	if err := service.Warm(context.Background()); err != nil {
		t.Fatal(err)
	}
	if entries := service.Counts(); entries != 1 {
		t.Fatalf("entries = %d", entries)
	}
	if _, err := service.Authorize(context.Background(), "host", "device"); err != nil {
		t.Fatalf("warm authorize: %v", err)
	}
}

func TestDenyIgnoresEmptyScopesAndCheckHandlesDenials(t *testing.T) {
	source := newFakeStore()
	source.put("host", "device", true, 1)
	service := newService(t, source, Config{})
	service.Deny("", "device")
	service.Deny("host", "")
	service.DenyDevice("")
	if service.Denied("", "device") || service.Denied("host", "") {
		t.Fatal("empty deny scope was installed")
	}
	if allowed, known := service.Check("", "device"); allowed || !known {
		t.Fatalf("empty Check = %v/%v", allowed, known)
	}
	service.Deny("host", "device")
	if allowed, known := service.Check("host", "device"); allowed || !known {
		t.Fatalf("denied Check = %v/%v", allowed, known)
	}
	if got := testutil.ToFloat64(service.metrics.AdmissionDenied); got != 1 {
		t.Fatalf("denied metric = %v", got)
	}
}

func TestPairingCompletionCannotClearPendingRevocation(t *testing.T) {
	for _, scope := range []string{"host", "device"} {
		t.Run(scope, func(t *testing.T) {
			const hostID, deviceID = "host", "device"
			source := newFakeStore()
			source.put(hostID, deviceID, true, 1)
			service := newService(t, source, Config{})

			var finish func()
			if scope == "host" {
				first := service.BeginGrantRevoke(hostID, deviceID)
				finish = service.BeginGrantRevoke(hostID, deviceID)
				first()
			} else {
				service.DenyDevice(deviceID)
				finish = func() { service.CompleteDeviceRevoke(deviceID) }
			}
			if err := service.Allow(t.Context(), hostID, deviceID); err == nil {
				t.Fatal("pairing completion accepted a grant with a pending revocation")
			}
			if allowed, known := service.Check(hostID, deviceID); allowed || !known {
				t.Fatal("admission fence was lost before the durable revocation")
			}
			source.put(hostID, deviceID, false, 1)
			service.Deny(hostID, deviceID)
			finish()
			if err := service.Allow(t.Context(), hostID, deviceID); err == nil {
				t.Fatal("pairing completion accepted a revoked grant")
			}
			source.put(hostID, deviceID, true, 2)
			if err := service.Allow(t.Context(), hostID, deviceID); err != nil {
				t.Fatalf("new pairing after revocation was denied: %v", err)
			}
		})
	}
}

// TestDelayedPairingCompletionCannotReopenRevokedHostGrant pins the ordering
// rule that a Host-scoped revocation which commits while a pairing completion
// is mid-read wins. The completion captured the old active grant before the
// fence was installed, so it must not clear that fence.
func TestDelayedPairingCompletionCannotReopenRevokedHostGrant(t *testing.T) {
	source := newFakeStore()
	source.put("revoked-host", "device", true, 1)
	source.put("other-host", "device", true, 1)
	service := newService(t, source, Config{LoadTimeout: 2 * time.Second})

	entered, release := source.blockKeySignal("revoked-host", "device")
	completion := make(chan error, 1)
	go func() {
		completion <- service.Allow(context.Background(), "revoked-host", "device")
	}()
	<-entered // the active grant is captured but the read has not returned.

	// Production order for a Host-scoped revoke: fence local admission, then
	// commit the durable revocation while the pairing read is still paused.
	finish := service.BeginGrantRevoke("revoked-host", "device")
	source.put("revoked-host", "device", false, 1)
	finish()
	close(release)

	if err := <-completion; err == nil {
		t.Fatal("delayed pairing completion cleared a committed revocation")
	}
	if allowed, err := service.Authorize(context.Background(), "revoked-host", "device"); err != nil || allowed {
		t.Fatalf("revoked host authorized after delayed completion: allowed=%v err=%v", allowed, err)
	}
	if allowed, err := service.Authorize(context.Background(), "other-host", "device"); err != nil || !allowed {
		t.Fatalf("unrelated host grant was denied: allowed=%v err=%v", allowed, err)
	}

	// Positive control: a separately authorized newer generation (a fresh
	// pairing) may clear the earlier fence.
	source.put("revoked-host", "device", true, 2)
	if err := service.Allow(context.Background(), "revoked-host", "device"); err != nil {
		t.Fatalf("newer pairing generation was not usable: %v", err)
	}
	if allowed, err := service.Authorize(context.Background(), "revoked-host", "device"); err != nil || !allowed {
		t.Fatalf("newer generation did not authorize: allowed=%v err=%v", allowed, err)
	}
}

// TestDelayedPairingCompletionCannotEscapeDeviceWideFence covers the
// device-wide revocation variant: the provisional device fence and the
// confirmed per-grant denials must both survive the delayed completion.
func TestDelayedPairingCompletionCannotEscapeDeviceWideFence(t *testing.T) {
	source := newFakeStore()
	source.put("host-a", "device", true, 1)
	source.put("host-b", "device", true, 1)
	service := newService(t, source, Config{LoadTimeout: 2 * time.Second})

	entered, release := source.blockKeySignal("host-a", "device")
	completion := make(chan error, 1)
	go func() {
		completion <- service.Allow(context.Background(), "host-a", "device")
	}()
	<-entered

	// Production order for a device-wide revoke: provisional device fence,
	// durable commit of every grant, confirmed per-grant denials, then narrow
	// the provisional fence.
	service.DenyDevice("device")
	source.put("host-a", "device", false, 1)
	source.put("host-b", "device", false, 1)
	service.Deny("host-a", "device")
	service.Deny("host-b", "device")
	service.CompleteDeviceRevoke("device")
	close(release)

	if err := <-completion; err == nil {
		t.Fatal("delayed pairing completion escaped a device-wide revocation")
	}
	for _, hostID := range []string{"host-a", "host-b"} {
		if allowed, err := service.Authorize(context.Background(), hostID, "device"); err != nil || allowed {
			t.Fatalf("%s authorized after device-wide revocation: allowed=%v err=%v", hostID, allowed, err)
		}
	}

	// Positive control: a newer pairing generation for one host is usable
	// after the device-wide revocation completed.
	source.put("host-a", "device", true, 2)
	if err := service.Allow(context.Background(), "host-a", "device"); err != nil {
		t.Fatalf("newer generation after device-wide revocation: %v", err)
	}
	if allowed, err := service.Authorize(context.Background(), "host-a", "device"); err != nil || !allowed {
		t.Fatalf("newer generation did not authorize: allowed=%v err=%v", allowed, err)
	}
	if allowed, _ := service.Authorize(context.Background(), "host-b", "device"); allowed {
		t.Fatal("unrelated revoked grant reopened after device-wide revocation")
	}
}
