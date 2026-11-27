// Package admission owns the bounded local grant state used by realtime
// authorization. PostgreSQL remains authoritative; this package decides
// whether a warmed grant still applies and keeps scoped denials outside the
// evictable cache so a late load cannot reopen a revoked grant.
package admission

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"sync"
	"time"

	"github.com/maypok86/otter/v2"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/store"
)

// Grant is a cached authorization revision.
type Grant struct {
	HostID                     string
	DeviceID                   string
	Active                     bool
	GrantGeneration            uint64
	RevocationGeneration       uint64
	DeviceRevocationGeneration uint64
}

func grantFromStore(item store.AdmissionGrant) Grant {
	return Grant{
		HostID:                     item.HostID,
		DeviceID:                   item.DeviceID,
		Active:                     item.Active,
		GrantGeneration:            item.GrantGeneration,
		RevocationGeneration:       item.RevocationGeneration,
		DeviceRevocationGeneration: item.DeviceRevocationGeneration,
	}
}

// Key identifies one Host/device grant.
type Key struct {
	HostID   string
	DeviceID string
}

// Store is the durable grant source used on cache misses and reconciliation.
type Store interface {
	AdmissionGrantFor(ctx context.Context, hostID, deviceID string) (store.AdmissionGrant, error)
	ListAdmissionGrants(ctx context.Context) ([]store.AdmissionGrant, error)
}

// Config bounds the local grant state.
type Config struct {
	MaximumEntries int
	TTL            time.Duration
	LoadTimeout    time.Duration
	// MaxBackgroundLoads bounds asynchronous cold loads started by Check.
	MaxBackgroundLoads int
}

// Dependencies wires an admission service.
type Dependencies struct {
	Store   Store
	Config  Config
	Metrics *diagnostics.Metrics
	Masker  *diagnostics.Masker
	Logger  *slog.Logger
}

// Service implements local grant admission.
type Service struct {
	store   Store
	cache   *otter.Cache[Key, Grant]
	cfg     Config
	metrics *diagnostics.Metrics
	masker  *diagnostics.Masker
	logger  *slog.Logger

	mu             sync.RWMutex
	denies         map[Key]struct{}
	deviceDenies   map[string]struct{}
	keyFences      map[Key]uint64
	deviceFences   map[string]uint64
	grantRevokes   map[Key]int
	reconcileEpoch uint64
	loadSemaphore  chan struct{}
}

// New constructs an admission service.
func New(deps Dependencies) (*Service, error) {
	if deps.Store == nil {
		return nil, errors.New("admission: store is required")
	}
	if deps.Logger == nil {
		deps.Logger = slog.Default()
	}
	if deps.Metrics == nil {
		deps.Metrics = diagnostics.NewMetrics()
	}
	if deps.Masker == nil {
		deps.Masker = diagnostics.NewMasker()
	}
	if deps.Config.MaximumEntries <= 0 {
		deps.Config.MaximumEntries = 10000
	}
	if deps.Config.TTL <= 0 {
		deps.Config.TTL = 5 * time.Minute
	}
	if deps.Config.LoadTimeout <= 0 {
		deps.Config.LoadTimeout = 250 * time.Millisecond
	}
	if deps.Config.MaxBackgroundLoads <= 0 {
		deps.Config.MaxBackgroundLoads = 64
	}
	cache, err := otter.New(&otter.Options[Key, Grant]{
		MaximumSize:      deps.Config.MaximumEntries,
		ExpiryCalculator: otter.ExpiryCreating[Key, Grant](deps.Config.TTL),
	})
	if err != nil {
		return nil, fmt.Errorf("admission: create cache: %w", err)
	}
	return &Service{
		store:         deps.Store,
		cache:         cache,
		cfg:           deps.Config,
		metrics:       deps.Metrics,
		masker:        deps.Masker,
		logger:        deps.Logger,
		denies:        map[Key]struct{}{},
		deviceDenies:  map[string]struct{}{},
		keyFences:     map[Key]uint64{},
		deviceFences:  map[string]uint64{},
		grantRevokes:  map[Key]int{},
		loadSemaphore: make(chan struct{}, deps.Config.MaxBackgroundLoads),
	}, nil
}

func (s *Service) deniedLocked(key Key) bool {
	if _, ok := s.denies[key]; ok {
		return true
	}
	_, ok := s.deviceDenies[key.DeviceID]
	return ok
}

// denied reports whether a scoped denial currently covers the grant.
func (s *Service) denied(key Key) bool {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.deniedLocked(key)
}

func (s *Service) loader(ctx context.Context, key Key) (Grant, error) {
	loadCtx, cancel := context.WithTimeout(ctx, s.cfg.LoadTimeout)
	defer cancel()
	if s.metrics != nil {
		s.metrics.AdmissionLoads.Inc()
	}
	item, err := s.store.AdmissionGrantFor(loadCtx, key.HostID, key.DeviceID)
	if err != nil {
		if errors.Is(err, store.ErrNotFound) {
			return Grant{HostID: key.HostID, DeviceID: key.DeviceID}, nil
		}
		if s.metrics != nil {
			s.metrics.AdmissionLoadErrors.Inc()
		}
		s.logger.Warn("grant load failed",
			"host", s.masker.Host(key.HostID), "device", s.masker.Device(key.DeviceID), "error", err.Error())
		return Grant{HostID: key.HostID, DeviceID: key.DeviceID}, fmt.Errorf("admission: load grant: %w", err)
	}
	return grantFromStore(item), nil
}

// Authorize returns whether the current durable grant applies. A cache hit
// performs no database work. Concurrent misses for one key share one load.
func (s *Service) Authorize(ctx context.Context, hostID, deviceID string) (bool, error) {
	if hostID == "" || deviceID == "" {
		return false, nil
	}
	key := Key{HostID: hostID, DeviceID: deviceID}
	if s.denied(key) {
		s.countDenied()
		return false, nil
	}
	grant, err := s.cache.Get(ctx, key, otter.LoaderFunc[Key, Grant](s.loader))
	if err != nil {
		if s.metrics != nil {
			s.metrics.AdmissionLoadErrors.Inc()
		}
		return false, fmt.Errorf("admission: load grant: %w", err)
	}
	if !grant.Active || s.denied(key) {
		s.countDenied()
		return false, nil
	}
	if s.metrics != nil {
		s.metrics.AdmissionHits.Inc()
	}
	return true, nil
}

// Check reports a local decision without waiting for a cold load. When the
// grant is not cached it reports unknown and starts a bounded background load
// so the realtime path never blocks on a database lookup.
func (s *Service) Check(hostID, deviceID string) (allowed bool, known bool) {
	if hostID == "" || deviceID == "" {
		return false, true
	}
	key := Key{HostID: hostID, DeviceID: deviceID}
	if s.denied(key) {
		s.countDenied()
		return false, true
	}
	grant, ok := s.cache.GetIfPresent(key)
	if !ok {
		s.startBackgroundLoad(key)
		return false, false
	}
	allowed = grant.Active && !s.denied(key)
	if allowed {
		if s.metrics != nil {
			s.metrics.AdmissionHits.Inc()
		}
	} else {
		s.countDenied()
	}
	return allowed, true
}

func (s *Service) startBackgroundLoad(key Key) {
	select {
	case s.loadSemaphore <- struct{}{}:
	default:
		return
	}
	go func() {
		defer func() { <-s.loadSemaphore }()
		_, _ = s.cache.Get(context.Background(), key, otter.LoaderFunc[Key, Grant](s.loader))
	}()
}

func (s *Service) countDenied() {
	if s.metrics != nil {
		s.metrics.AdmissionDenied.Inc()
	}
}

// Warm loads authoritative grant state before external traffic is admitted.
func (s *Service) Warm(ctx context.Context) error {
	return s.Reconcile(ctx)
}

// Reconcile rebuilds cached grant state from PostgreSQL. Revoked grants install
// scoped denials; a local denial or device fence is never cleared here because
// it may fence a durable write that is still in flight.
func (s *Service) Reconcile(ctx context.Context) error {
	items, err := s.store.ListAdmissionGrants(ctx)
	if err != nil {
		if s.metrics != nil {
			s.metrics.AdmissionReconcileErrors.Inc()
		}
		return fmt.Errorf("admission: reconcile: %w", err)
	}
	active := make(map[Key]Grant, len(items))
	for _, item := range items {
		grant := grantFromStore(item)
		key := Key{HostID: grant.HostID, DeviceID: grant.DeviceID}
		active[key] = grant
	}
	s.mu.Lock()
	s.reconcileEpoch++
	for key, grant := range active {
		if !grant.Active {
			s.denies[key] = struct{}{}
		}
	}
	// A local denial or device fence is installed before the durable write.
	// Reconcile must not clear it while that write may still be in flight;
	// only Allow (after a confirmed pairing) or CompleteDeviceRevoke may.
	s.mu.Unlock()

	s.cache.InvalidateAll()
	for key, grant := range active {
		// Inactive grants are denied through the non-evictable denial map. Do
		// not let them consume the bounded hot-grant cache.
		if grant.Active {
			s.cache.Set(key, grant)
		}
	}
	s.logger.Debug("grant admission reconciled", "grants", len(active))
	return nil
}

// Allow installs a confirmed pairing or repair. The durable grant is re-read so
// only committed state can clear a denial.
//
// A pending revocation can leave the durable row active until it commits.
// Fence versions also reject a read that overlapped a revocation after it ends.
func (s *Service) Allow(ctx context.Context, hostID, deviceID string) error {
	key := Key{HostID: hostID, DeviceID: deviceID}
	s.mu.RLock()
	keyFence := s.keyFences[key]
	deviceFence := s.deviceFences[key.DeviceID]
	reconcileEpoch := s.reconcileEpoch
	_, deviceRevoking := s.deviceDenies[key.DeviceID]
	revoking := s.grantRevokes[key] > 0 || deviceRevoking
	s.mu.RUnlock()
	if revoking {
		return errors.New("admission: pairing completion raced a revocation")
	}
	// Read directly so an active row cannot enter the cache before the fence check.
	s.cache.Invalidate(key)
	grant, err := s.loader(ctx, key)
	if err != nil {
		return fmt.Errorf("admission: reload paired grant: %w", err)
	}
	if !grant.Active {
		return errors.New("admission: paired grant is not active")
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.keyFences[key] != keyFence || s.deviceFences[key.DeviceID] != deviceFence ||
		s.reconcileEpoch != reconcileEpoch {
		return errors.New("admission: pairing completion raced a revocation or reconcile")
	}
	delete(s.denies, key)
	s.cache.Set(key, grant)
	return nil
}

// BeginGrantRevoke fences a grant for the duration of a Host-scoped durable
// write. Finishing the write leaves the denial in place, including on failure.
func (s *Service) BeginGrantRevoke(hostID, deviceID string) func() {
	if hostID == "" || deviceID == "" {
		return func() {}
	}
	key := Key{HostID: hostID, DeviceID: deviceID}
	s.mu.Lock()
	s.grantRevokes[key]++
	s.denies[key] = struct{}{}
	s.keyFences[key]++
	s.mu.Unlock()
	return func() {
		s.mu.Lock()
		defer s.mu.Unlock()
		s.grantRevokes[key]--
		if s.grantRevokes[key] == 0 {
			delete(s.grantRevokes, key)
		}
	}
}

// Deny fences one Host/device grant immediately.
func (s *Service) Deny(hostID, deviceID string) {
	if hostID == "" || deviceID == "" {
		return
	}
	key := Key{HostID: hostID, DeviceID: deviceID}
	s.mu.Lock()
	s.denies[key] = struct{}{}
	s.keyFences[key]++
	s.mu.Unlock()
}

// DenyDevice fences every grant of a device while a device-wide revocation
// commits. CompleteDeviceRevoke narrows it to the confirmed grant scopes.
func (s *Service) DenyDevice(deviceID string) {
	if deviceID == "" {
		return
	}
	s.mu.Lock()
	s.deviceDenies[deviceID] = struct{}{}
	s.deviceFences[deviceID]++
	s.mu.Unlock()
}

// CompleteDeviceRevoke narrows a provisional device fence after the confirmed
// per-grant denials are installed.
func (s *Service) CompleteDeviceRevoke(deviceID string) {
	s.mu.Lock()
	delete(s.deviceDenies, deviceID)
	s.deviceFences[deviceID]++
	s.mu.Unlock()
}

// Denied reports whether a scoped denial is currently installed. Tests and
// diagnostics use it; it never authorizes.
func (s *Service) Denied(hostID, deviceID string) bool {
	return s.denied(Key{HostID: hostID, DeviceID: deviceID})
}

// Counts exposes local cache counters for tests and metrics wiring.
func (s *Service) Counts() (entries int) {
	return s.cache.EstimatedSize()
}
