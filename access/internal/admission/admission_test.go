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
	err     error
	listErr error
}

func newFakeStore() *fakeStore {
	return &fakeStore{
		grants: map[Key]store.AdmissionGrant{},
		loads:  map[Key]int{},
		block:  map[Key]chan struct{}{},
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

func (f *fakeStore) AdmissionGrantFor(ctx context.Context, hostID, deviceID string) (store.AdmissionGrant, error) {
	key := Key{HostID: hostID, DeviceID: deviceID}
	f.mu.Lock()
	f.loads[key]++
	f.total++
	release := f.block[key]
	err := f.err
	grant, ok := f.grants[key]
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

func TestReconcileRestoresAuthorityFromDurableState(t *testing.T) {
	source := newFakeStore()
	source.put("host", "device", true, 4)
	source.put("other", "device", false, 2)
	service := newService(t, source, Config{})
	service.Deny("host", "device")
	service.DenyDevice("device")

	if err := service.Reconcile(context.Background()); err != nil {
		t.Fatalf("reconcile: %v", err)
	}
	if allowed, err := service.Authorize(context.Background(), "host", "device"); err != nil || !allowed {
		t.Fatalf("active grant not restored: allowed=%v err=%v", allowed, err)
	}
	if allowed, err := service.Authorize(context.Background(), "other", "device"); err != nil || allowed {
		t.Fatalf("revoked grant reopened: allowed=%v err=%v", allowed, err)
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
	if entries := service.Counts(); entries != 2 {
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
