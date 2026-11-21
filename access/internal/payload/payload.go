// Package payload owns temporary ciphertext admission on one Access node.
//
// PostgreSQL records the authoritative reservation, scope, hard expiry and
// reader leases; this package stages the opaque file in one dedicated quota
// directory and composes the store statements. It never inspects ciphertext,
// never logs a path or object id, and publishes an object only after the file
// is complete, fsynced, verified and atomically renamed.
package payload

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/store"
)

// Typed admission and lifecycle errors. The HTTP layer maps these to stable
// wire outcomes; callers should use errors.Is rather than string comparison.
var (
	ErrDisabled       = errors.New("temporary payload storage is disabled")
	ErrInvalid        = errors.New("invalid payload request")
	ErrCapacity       = errors.New("payload node capacity exhausted")
	ErrTooLarge       = errors.New("payload exceeds the object limit")
	ErrQuota          = errors.New("payload host quota exhausted")
	ErrHeadroom       = errors.New("filesystem headroom below the configured reserve")
	ErrDuplicate      = errors.New("payload object already exists")
	ErrNotFound       = errors.New("payload object not found")
	ErrExpired        = errors.New("payload object expired")
	ErrForbidden      = errors.New("payload scope is not authorized")
	ErrNotReady       = errors.New("payload object is not ready")
	ErrNotStaged      = errors.New("payload body is not staged")
	ErrLengthMismatch = errors.New("payload body length does not match the reservation")
	ErrDigestMismatch = errors.New("payload body digest does not match the reservation")
)

// MaxReadersPerObject bounds unexpired reader leases for one object.
const MaxReadersPerObject = 32

// Object is one persisted payload metadata row.
type Object = store.PayloadObject

var objectIDPattern = regexp.MustCompile(`^[0-9a-f]{32}$`)

// Config bounds one node's temporary ciphertext store.
type Config struct {
	Enabled               bool
	RootDir               string
	NodeID                string
	OwnerID               string
	MaxObjectBytes        int64
	MaxHostBytes          int64
	MaxNodeBytes          int64
	MaxObjects            int
	MaxActiveUploads      int
	FreeSpaceReserveBytes int64
	Retention             time.Duration
	CleanupInterval       time.Duration
	CleanupBatch          int
	UploadTimeout         time.Duration
	ReadTimeout           time.Duration
}

// Validate rejects zero, negative and inconsistent capacity settings. The root
// directory is checked for writability by New, where the service owns it.
func (c Config) Validate() error {
	if !c.Enabled {
		return nil
	}
	var problems []string
	add := func(condition bool, message string) {
		if condition {
			problems = append(problems, message)
		}
	}
	add(strings.TrimSpace(c.RootDir) == "" || !filepath.IsAbs(c.RootDir), "storage.root_dir must be an absolute path")
	add(strings.TrimSpace(c.NodeID) == "", "storage.node_id is required")
	add(c.MaxObjectBytes <= 0 || c.MaxHostBytes <= 0 || c.MaxNodeBytes <= 0, "storage byte limits must be positive")
	add(c.MaxHostBytes < c.MaxObjectBytes, "storage.max_host_bytes must be at least storage.max_object_bytes")
	add(c.MaxNodeBytes < c.MaxHostBytes, "storage.max_node_bytes must be at least storage.max_host_bytes")
	add(c.MaxObjects <= 0, "storage.max_objects must be positive")
	add(c.MaxActiveUploads <= 0 || c.MaxActiveUploads > c.MaxObjects, "storage.max_active_uploads must be positive and at most storage.max_objects")
	add(c.FreeSpaceReserveBytes <= 0, "storage.free_space_reserve_bytes must be positive")
	add(c.Retention <= 0, "storage.retention must be positive")
	add(c.CleanupInterval <= 0, "storage.cleanup_interval must be positive")
	add(c.CleanupBatch <= 0, "storage.cleanup_batch must be positive")
	add(c.UploadTimeout <= 0, "storage.upload_timeout must be positive")
	add(c.ReadTimeout <= 0, "storage.read_timeout must be positive")
	add(c.UploadTimeout > c.Retention, "storage.upload_timeout must not exceed storage.retention")
	if len(problems) > 0 {
		return errors.New("invalid storage config: " + strings.Join(problems, "; "))
	}
	return nil
}

// Store is the SQL seam this package composes. All statements remain in the
// store package.
type Store interface {
	ReservePayload(context.Context, store.PayloadReservation, store.PayloadLimits) (store.PayloadAdmission, error)
	GetPayload(context.Context, string) (store.PayloadObject, error)
	ConfirmPayloadReady(context.Context, string, int64, []byte, time.Time) (store.PayloadObject, error)
	OpenPayloadRead(context.Context, store.PayloadReadScope, string, time.Time) (store.PayloadObject, error)
	ReleasePayloadRead(context.Context, string, string) error
	CancelPayload(context.Context, store.PayloadReadScope, time.Time) (store.PayloadObject, error)
	ExpirePayloads(context.Context, time.Time, int) ([]store.PayloadObject, error)
	ClaimPayloadCleanup(context.Context, time.Time, int, time.Duration) ([]store.PayloadObject, error)
	MarkPayloadCleanupFailure(context.Context, string, time.Time, time.Duration) error
	MarkPayloadDeleted(context.Context, string, time.Time) (bool, error)
	DeletePayloadTombstones(context.Context, time.Time, int) (int64, error)
	StaleReservedPayloads(context.Context, time.Time, int) ([]store.PayloadObject, error)
	ReadyPayloadPage(context.Context, string, int) ([]store.PayloadObject, error)
	PayloadStats(context.Context, time.Time) (store.PayloadStats, error)
}

// Dependencies are the service seams.
type Dependencies struct {
	Store   Store
	Config  Config
	Metrics *diagnostics.Metrics
	Logger  *slog.Logger
	// Now overrides the wall clock in tests. Retries and fetches never move a
	// stored expiry, so clock injection only affects new decisions.
	Now func() time.Time
}

// Ref identifies one ciphertext object for a write, finish, read or cancel.
// Empty scope fields are not checked; the HTTP layer passes the authenticated
// scope it already validated.
type Ref struct {
	ObjectID   string
	HostID     string
	DeviceID   string
	AppSession string
	ChannelID  string
	Purpose    string
}

// ReserveInput is one admission request. ObjectID is generated when empty.
// ParentDeadline bounds the node retention cap; the earlier instant wins.
type ReserveInput struct {
	ObjectID                   string
	HostID                     string
	DeviceID                   string
	AppSession                 string
	ChannelID                  string
	RequestID                  string
	MessageID                  string
	EventID                    string
	Purpose                    string
	PartID                     string
	Length                     int64
	ReservedBytes              int64
	Digest                     []byte
	ParentDeadline             time.Time
	GrantGeneration            *int64
	RevocationGeneration       *int64
	DeviceRevocationGeneration *int64
}

// Stats is one operator snapshot.
type Stats struct {
	ReservedObjects      int
	ReadyObjects         int
	ExpiredObjects       int
	DeletedObjects       int
	ChargedBytes         int64
	ReservedBytes        int64
	ReadyBytes           int64
	ExpiredBytes         int64
	ActiveUploads        int
	ReaderLeases         int
	OldestExpiredBacklog time.Duration
	FreeBytes            int64
	HeadroomBytes        int64
}

// CleanupResult reports one bounded cleanup pass.
type CleanupResult struct {
	Expired        int
	Deleted        int
	ReclaimedBytes int64
	Failed         int
	Tombstones     int64
}

// Service composes the payload store and the node quota directory.
type Service struct {
	store   Store
	config  Config
	metrics *diagnostics.Metrics
	logger  *slog.Logger
	now     func() time.Time

	root    string
	objects string
	tmp     string

	mu        sync.Mutex
	staged    map[string]string
	cancelled map[string]struct{}
}

// New validates configuration, prepares the quota directory and returns a
// service. An unsupported platform fails closed instead of silently skipping
// the free-space reserve.
func New(deps Dependencies) (*Service, error) {
	if deps.Store == nil {
		return nil, errors.New("payload: store is required")
	}
	if err := deps.Config.Validate(); err != nil {
		return nil, err
	}
	now := deps.Now
	if now == nil {
		now = time.Now
	}
	logger := deps.Logger
	if logger == nil {
		logger = slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelError}))
	}
	service := &Service{
		store:     deps.Store,
		config:    deps.Config,
		metrics:   deps.Metrics,
		logger:    logger,
		now:       now,
		staged:    make(map[string]string),
		cancelled: make(map[string]struct{}),
	}
	if !deps.Config.Enabled {
		return service, nil
	}
	service.root = filepath.Clean(deps.Config.RootDir)
	service.objects = filepath.Join(service.root, "objects")
	service.tmp = filepath.Join(service.root, "tmp")
	for _, dir := range []string{service.root, service.objects, service.tmp} {
		if err := os.MkdirAll(dir, 0o750); err != nil {
			return nil, fmt.Errorf("prepare storage directory: %w", sanitizeError(err))
		}
	}
	if err := checkWritable(service.tmp); err != nil {
		return nil, fmt.Errorf("storage directory is not writable: %w", sanitizeError(err))
	}
	if _, err := freeBytes(service.root); err != nil {
		return nil, fmt.Errorf("storage filesystem headroom: %w", sanitizeError(err))
	}
	return service, nil
}

// Enabled reports whether the operator enabled temporary storage.
func (s *Service) Enabled() bool { return s != nil && s.config.Enabled }

// Ready verifies the quota directory is writable and still has its reserve.
func (s *Service) Ready(ctx context.Context) error {
	if !s.Enabled() {
		return nil
	}
	if err := ctx.Err(); err != nil {
		return err
	}
	if err := checkWritable(s.tmp); err != nil {
		return err
	}
	free, err := freeBytes(s.root)
	if err != nil {
		return sanitizeError(err)
	}
	if free < s.config.FreeSpaceReserveBytes {
		return ErrHeadroom
	}
	return nil
}

// Reserve admits one object before any upload body is consumed.
func (s *Service) Reserve(ctx context.Context, input ReserveInput) (Object, error) {
	if !s.Enabled() {
		return Object{}, ErrDisabled
	}
	if err := s.validateReserve(input); err != nil {
		return Object{}, err
	}
	now := s.now().UTC()
	expires := now.Add(s.config.Retention)
	if !input.ParentDeadline.IsZero() {
		if !input.ParentDeadline.After(now) {
			return Object{}, fmt.Errorf("%w: parent deadline has passed", ErrExpired)
		}
		if input.ParentDeadline.Before(expires) {
			expires = input.ParentDeadline
		}
	}
	objectID := input.ObjectID
	if objectID == "" {
		generated, err := randomID()
		if err != nil {
			return Object{}, err
		}
		objectID = generated
	}
	reserved := input.ReservedBytes
	if reserved == 0 {
		reserved = input.Length
	}
	stats, err := s.store.PayloadStats(ctx, now)
	if err != nil {
		return Object{}, err
	}
	free, err := freeBytes(s.root)
	if err != nil {
		return Object{}, fmt.Errorf("filesystem headroom: %w", sanitizeError(err))
	}
	if free-s.config.FreeSpaceReserveBytes-stats.ReservedBytes-reserved < 0 {
		s.refused("headroom")
		return Object{}, ErrHeadroom
	}
	admission, err := s.store.ReservePayload(ctx, store.PayloadReservation{
		ObjectID:                   objectID,
		NodeID:                     s.config.NodeID,
		OwnerID:                    s.ownerID(),
		HostID:                     input.HostID,
		DeviceID:                   input.DeviceID,
		AppSession:                 input.AppSession,
		ChannelID:                  input.ChannelID,
		RequestID:                  input.RequestID,
		MessageID:                  input.MessageID,
		EventID:                    input.EventID,
		Purpose:                    input.Purpose,
		PartID:                     input.PartID,
		Length:                     input.Length,
		ReservedBytes:              reserved,
		Digest:                     input.Digest,
		ExpiresAt:                  expires,
		GrantGeneration:            input.GrantGeneration,
		RevocationGeneration:       input.RevocationGeneration,
		DeviceRevocationGeneration: input.DeviceRevocationGeneration,
	}, store.PayloadLimits{
		MaxObjectBytes:   s.config.MaxObjectBytes,
		MaxHostBytes:     s.config.MaxHostBytes,
		MaxNodeBytes:     s.config.MaxNodeBytes,
		MaxObjects:       s.config.MaxObjects,
		MaxActiveUploads: s.config.MaxActiveUploads,
	})
	if err != nil {
		if errors.Is(err, store.ErrPayloadDuplicate) {
			s.refused("duplicate")
			return Object{}, fmt.Errorf("%w: %s", ErrDuplicate, objectID)
		}
		return Object{}, err
	}
	if admission.Reason != "" {
		s.refused(admission.Reason)
		switch admission.Reason {
		case store.PayloadRefusedSize:
			return Object{}, fmt.Errorf("%w: %d bytes", ErrTooLarge, reserved)
		case store.PayloadRefusedQuota:
			return Object{}, fmt.Errorf("%w: host limit reached", ErrQuota)
		default:
			return Object{}, fmt.Errorf("%w: node limit reached", ErrCapacity)
		}
	}
	return admission.Object, nil
}

func (s *Service) validateReserve(input ReserveInput) error {
	switch {
	case input.ObjectID != "" && !objectIDPattern.MatchString(input.ObjectID):
		return fmt.Errorf("%w: object id must be 32 lowercase hex digits", ErrInvalid)
	case strings.TrimSpace(input.HostID) == "":
		return fmt.Errorf("%w: host id is required", ErrInvalid)
	case strings.TrimSpace(input.Purpose) == "":
		return fmt.Errorf("%w: purpose is required", ErrInvalid)
	case countParents(input.RequestID, input.MessageID, input.EventID) != 1:
		return fmt.Errorf("%w: exactly one request, message or event identity is required", ErrInvalid)
	case input.AppSession != "" && input.DeviceID == "":
		return fmt.Errorf("%w: an app session requires a device id", ErrInvalid)
	case input.Length <= 0:
		return fmt.Errorf("%w: length must be positive", ErrInvalid)
	case input.ReservedBytes < 0 || (input.ReservedBytes > 0 && input.ReservedBytes < input.Length):
		return fmt.Errorf("%w: reserved bytes must cover the length", ErrInvalid)
	case len(input.Digest) != sha256.Size:
		return fmt.Errorf("%w: digest must be SHA-256", ErrInvalid)
	}
	if input.Length > s.config.MaxObjectBytes {
		s.refused("size")
		return fmt.Errorf("%w: %d bytes", ErrTooLarge, input.Length)
	}
	return nil
}

func countParents(ids ...string) int {
	count := 0
	for _, id := range ids {
		if id != "" {
			count++
		}
	}
	return count
}

func (s *Service) ownerID() string {
	if s.config.OwnerID != "" {
		return s.config.OwnerID
	}
	return s.config.NodeID
}

func (s *Service) refused(reason string) {
	if s.metrics != nil {
		s.metrics.PayloadRefused(reason)
	}
}

// Cancel closes admission for an object and enqueues bounded cleanup. A staged
// body in this process is removed immediately; a committed file is left for
// cleanup so an active reader keeps its charge until it closes.
func (s *Service) Cancel(ctx context.Context, ref Ref) error {
	if !s.Enabled() {
		return ErrDisabled
	}
	if err := validateRef(ref); err != nil {
		return err
	}
	_, err := s.store.CancelPayload(ctx, s.readScope(ref), s.now().UTC())
	if errors.Is(err, store.ErrPayloadScopeMismatch) {
		return ErrForbidden
	}
	if errors.Is(err, store.ErrNotFound) {
		return ErrNotFound
	}
	if err != nil {
		return err
	}
	s.discardStaged(ref.ObjectID)
	s.markCancelled(ref.ObjectID)
	s.observe(ctx)
	return nil
}

// markCancelled interrupts an already-open reader for this object. A reader
// that opened before expiry or revocation stops at its next Read instead of
// serving the full lease; the bytes stay charged until Close.
func (s *Service) markCancelled(objectID string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.cancelled == nil {
		s.cancelled = make(map[string]struct{})
	}
	s.cancelled[objectID] = struct{}{}
}

func (s *Service) isCancelled(objectID string) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	_, ok := s.cancelled[objectID]
	return ok
}

func (s *Service) clearCancelled(objectID string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	delete(s.cancelled, objectID)
}

// Stats returns the current accounting and filesystem headroom.
func (s *Service) Stats(ctx context.Context) (Stats, error) {
	if !s.Enabled() {
		return Stats{}, ErrDisabled
	}
	now := s.now().UTC()
	persisted, err := s.store.PayloadStats(ctx, now)
	if err != nil {
		return Stats{}, err
	}
	free, err := freeBytes(s.root)
	if err != nil {
		return Stats{}, sanitizeError(err)
	}
	stats := Stats{
		ReservedObjects:      persisted.ReservedObjects,
		ReadyObjects:         persisted.ReadyObjects,
		ExpiredObjects:       persisted.ExpiredObjects,
		DeletedObjects:       persisted.DeletedObjects,
		ChargedBytes:         persisted.ChargedBytes,
		ReservedBytes:        persisted.ReservedBytes,
		ReadyBytes:           persisted.ReadyBytes,
		ExpiredBytes:         persisted.ExpiredBytes,
		ActiveUploads:        persisted.ActiveUploads,
		ReaderLeases:         persisted.ReaderLeases,
		OldestExpiredBacklog: time.Duration(persisted.OldestExpiredBacklog) * time.Second,
		FreeBytes:            free,
	}
	stats.HeadroomBytes = free - s.config.FreeSpaceReserveBytes - persisted.ReservedBytes
	if stats.HeadroomBytes < 0 {
		stats.HeadroomBytes = 0
	}
	s.publish(stats)
	return stats, nil
}

func (s *Service) publish(stats Stats) {
	if s.metrics == nil {
		return
	}
	s.metrics.PayloadStorage(stats.FreeBytes, stats.HeadroomBytes, stats.ReadyBytes,
		stats.ReservedBytes, stats.ExpiredBytes, stats.ReadyObjects,
		stats.ReservedObjects, stats.ActiveUploads,
		stats.OldestExpiredBacklog.Seconds())
}

func (s *Service) observe(ctx context.Context) {
	if s.metrics == nil {
		return
	}
	if _, err := s.Stats(ctx); err != nil {
		s.logger.Warn("payload stats unavailable", "error", sanitizeError(err).Error())
	}
}

func (s *Service) readScope(ref Ref) store.PayloadReadScope {
	return store.PayloadReadScope{
		ObjectID:   ref.ObjectID,
		HostID:     ref.HostID,
		DeviceID:   ref.DeviceID,
		AppSession: ref.AppSession,
		ChannelID:  ref.ChannelID,
		Purpose:    ref.Purpose,
	}
}

func validateRef(ref Ref) error {
	if !objectIDPattern.MatchString(ref.ObjectID) {
		return fmt.Errorf("%w: object id must be 32 lowercase hex digits", ErrInvalid)
	}
	return nil
}

func randomID() (string, error) {
	raw := make([]byte, 16)
	if _, err := rand.Read(raw); err != nil {
		return "", fmt.Errorf("payload id: %w", err)
	}
	return hex.EncodeToString(raw), nil
}

func (s *Service) finalPath(objectID string) string {
	return filepath.Join(s.objects, objectID[:2], objectID+".bin")
}

func (s *Service) newStagedPath(objectID string) (string, error) {
	suffix, err := randomID()
	if err != nil {
		return "", err
	}
	return filepath.Join(s.tmp, objectID+"."+suffix+".tmp"), nil
}

func (s *Service) stagedPath(objectID string) (string, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	path, ok := s.staged[objectID]
	return path, ok
}

func (s *Service) setStaged(objectID, path string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.staged[objectID] = path
}

func (s *Service) clearStaged(objectID string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	delete(s.staged, objectID)
}

func (s *Service) discardStaged(objectID string) {
	path, ok := s.stagedPath(objectID)
	if !ok {
		return
	}
	_ = os.Remove(path)
	s.clearStaged(objectID)
}

// DirEntryIDs extracts valid object ids from the managed directories so
// reconciliation can compare files with metadata.
func (s *Service) scanManagedFiles(limit int) ([]managedFile, error) {
	var files []managedFile
	read := func(dir string, match func(string) (string, bool)) error {
		entries, err := os.ReadDir(dir)
		if err != nil {
			if errors.Is(err, os.ErrNotExist) {
				return nil
			}
			return err
		}
		sort.Slice(entries, func(i, j int) bool { return entries[i].Name() < entries[j].Name() })
		for _, entry := range entries {
			if entry.IsDir() || len(files) >= limit {
				continue
			}
			if id, ok := match(entry.Name()); ok {
				files = append(files, managedFile{ObjectID: id, Path: filepath.Join(dir, entry.Name())})
			}
		}
		return nil
	}
	if err := read(s.tmp, matchTemporaryName); err != nil {
		return nil, err
	}
	subdirs, err := os.ReadDir(s.objects)
	if err != nil && !errors.Is(err, os.ErrNotExist) {
		return nil, err
	}
	for _, sub := range subdirs {
		if !sub.IsDir() || len(files) >= limit {
			continue
		}
		if err := read(filepath.Join(s.objects, sub.Name()), matchObjectName); err != nil {
			return nil, err
		}
	}
	return files, nil
}

type managedFile struct {
	ObjectID string
	Path     string
}

var (
	objectFilePattern = regexp.MustCompile(`^([0-9a-f]{32})\.bin$`)
	tempFilePattern   = regexp.MustCompile(`^([0-9a-f]{32})\.[0-9a-f]{32}\.tmp$`)
)

func matchObjectName(name string) (string, bool) {
	match := objectFilePattern.FindStringSubmatch(name)
	if match == nil {
		return "", false
	}
	return match[1], true
}

func matchTemporaryName(name string) (string, bool) {
	match := tempFilePattern.FindStringSubmatch(name)
	if match == nil {
		return "", false
	}
	return match[1], true
}

func checkWritable(dir string) error {
	file, err := os.CreateTemp(dir, ".write-probe-*")
	if err != nil {
		return sanitizeError(err)
	}
	name := file.Name()
	if err := file.Close(); err != nil {
		_ = os.Remove(name)
		return sanitizeError(err)
	}
	return sanitizeError(os.Remove(name))
}

// sanitizeError removes filesystem paths (and therefore object filenames and
// the quota root) before an error is returned to a caller that may log it. The
// underlying syscall cause is preserved for diagnosis.
func sanitizeError(err error) error {
	if err == nil {
		return nil
	}
	var pathErr *os.PathError
	if errors.As(err, &pathErr) {
		return fmt.Errorf("%s: %v", pathErr.Op, pathErr.Err)
	}
	var linkErr *os.LinkError
	if errors.As(err, &linkErr) {
		return fmt.Errorf("%s: %v", linkErr.Op, linkErr.Err)
	}
	return err
}

// Check reports whether a ready, unexpired object matches the authorized scope
// without acquiring a reader lease or reading ciphertext. Request admission,
// result notices and status reads use it to decide whether a reference can be
// advertised; the directory still rechecks authority on the actual fetch.
func (s *Service) Check(ctx context.Context, ref Ref) (Object, error) {
	if !s.Enabled() {
		return Object{}, ErrDisabled
	}
	if err := validateRef(ref); err != nil {
		return Object{}, err
	}
	now := s.now().UTC()
	object, err := s.store.GetPayload(ctx, ref.ObjectID)
	if err != nil {
		if errors.Is(err, store.ErrNotFound) {
			return Object{}, ErrNotFound
		}
		return Object{}, err
	}
	if !refMatches(ref, object) {
		return Object{}, ErrForbidden
	}
	switch object.State {
	case store.PayloadReady:
	case store.PayloadReserved:
		return Object{}, ErrNotReady
	case store.PayloadExpired, store.PayloadDeleted:
		return Object{}, ErrExpired
	default:
		return Object{}, ErrNotFound
	}
	if !object.ExpiresAt.After(now) {
		return Object{}, ErrExpired
	}
	return object, nil
}
