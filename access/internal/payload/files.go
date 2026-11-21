package payload

import (
	"bytes"
	"context"
	"crypto/sha256"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
)

// Write stages the complete body on the same volume as the final object. The
// read is capped at one byte beyond the reservation, so a missing or dishonest
// length cannot cause an unbounded read or write. The temp file is removed on
// any mismatch and is never readable.
func (s *Service) Write(ctx context.Context, ref Ref, body io.Reader) error {
	if !s.Enabled() {
		return ErrDisabled
	}
	if err := validateRef(ref); err != nil {
		return err
	}
	if body == nil {
		return fmt.Errorf("%w: body is required", ErrInvalid)
	}
	object, err := s.store.GetPayload(ctx, ref.ObjectID)
	if err != nil {
		if errors.Is(err, store.ErrNotFound) {
			return ErrNotFound
		}
		return err
	}
	if !refMatches(ref, object) {
		return ErrForbidden
	}
	now := s.now().UTC()
	if !object.ExpiresAt.After(now) {
		s.closeObject(ctx, object.ObjectID)
		return ErrExpired
	}
	if object.State != store.PayloadReserved {
		if object.State == store.PayloadExpired || object.State == store.PayloadDeleted {
			return ErrExpired
		}
		return ErrNotReady
	}
	s.discardStaged(object.ObjectID)
	tempPath, err := s.newStagedPath(object.ObjectID)
	if err != nil {
		return err
	}
	file, err := os.OpenFile(tempPath, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600)
	if err != nil {
		return fmt.Errorf("stage payload: %w", sanitizeError(err))
	}
	// The upload timeout bounds one staged write even when the caller's
	// context lives longer; a slow or stalled body is cancelled and its temp
	// file removed.
	writeCtx, cancelWrite := context.WithTimeout(ctx, s.config.UploadTimeout)
	defer cancelWrite()
	hasher := sha256.New()
	written, copyErr := io.Copy(io.MultiWriter(file, hasher), io.LimitReader(contextReader{ctx: writeCtx, reader: body}, object.Length+1))
	syncErr := file.Sync()
	closeErr := file.Close()
	if copyErr != nil {
		_ = os.Remove(tempPath)
		return fmt.Errorf("stage payload: %w", copyErr)
	}
	if syncErr != nil {
		_ = os.Remove(tempPath)
		return fmt.Errorf("stage payload: %w", sanitizeError(syncErr))
	}
	if closeErr != nil {
		_ = os.Remove(tempPath)
		return fmt.Errorf("stage payload: %w", sanitizeError(closeErr))
	}
	if written != object.Length {
		_ = os.Remove(tempPath)
		return fmt.Errorf("%w: got %d bytes, want %d", ErrLengthMismatch, written, object.Length)
	}
	if !bytes.Equal(hasher.Sum(nil), object.Digest) {
		_ = os.Remove(tempPath)
		return ErrDigestMismatch
	}
	s.setStaged(object.ObjectID, tempPath)
	return nil
}

// Finish verifies the staged body again, fsyncs it, atomically renames it onto
// the quota volume and only then commits the metadata to ready. If the metadata
// commit fails, the file remains committed on disk and reconciliation can
// publish it without exposing a partially written object.
func (s *Service) Finish(ctx context.Context, ref Ref) (Object, error) {
	if !s.Enabled() {
		return Object{}, ErrDisabled
	}
	if err := validateRef(ref); err != nil {
		return Object{}, err
	}
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
	now := s.now().UTC()
	if object.State == store.PayloadReady {
		if !object.ExpiresAt.After(now) {
			return Object{}, ErrExpired
		}
		return object, nil
	}
	if object.State != store.PayloadReserved {
		return Object{}, ErrExpired
	}
	if !object.ExpiresAt.After(now) {
		s.closeObject(ctx, object.ObjectID)
		return Object{}, ErrExpired
	}
	tempPath, staged := s.stagedPath(object.ObjectID)
	if !staged {
		// A prior Finish may have renamed the file but failed to commit the
		// metadata, or the process restarted after the rename.
		if _, statErr := os.Stat(s.finalPath(object.ObjectID)); statErr == nil {
			return s.publishCommitted(ctx, object)
		}
		return Object{}, ErrNotStaged
	}
	length, digest, err := fileDigest(tempPath)
	if err != nil {
		_ = os.Remove(tempPath)
		s.clearStaged(object.ObjectID)
		return Object{}, fmt.Errorf("verify staged payload: %w", sanitizeError(err))
	}
	if length != object.Length {
		_ = os.Remove(tempPath)
		s.clearStaged(object.ObjectID)
		return Object{}, fmt.Errorf("%w: got %d bytes, want %d", ErrLengthMismatch, length, object.Length)
	}
	if !bytes.Equal(digest, object.Digest) {
		_ = os.Remove(tempPath)
		s.clearStaged(object.ObjectID)
		return Object{}, ErrDigestMismatch
	}
	finalPath := s.finalPath(object.ObjectID)
	if err := os.MkdirAll(filepath.Dir(finalPath), 0o750); err != nil {
		return Object{}, fmt.Errorf("publish payload: %w", sanitizeError(err))
	}
	if err := os.Rename(tempPath, finalPath); err != nil {
		return Object{}, fmt.Errorf("publish payload: %w", sanitizeError(err))
	}
	if err := syncDirectory(filepath.Dir(finalPath)); err != nil {
		return Object{}, fmt.Errorf("publish payload: %w", sanitizeError(err))
	}
	s.clearStaged(object.ObjectID)
	return s.publishCommitted(ctx, object)
}

func (s *Service) publishCommitted(ctx context.Context, object Object) (Object, error) {
	length, digest, err := fileDigest(s.finalPath(object.ObjectID))
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			s.closeObject(ctx, object.ObjectID)
			return Object{}, ErrNotFound
		}
		return Object{}, fmt.Errorf("verify committed payload: %w", sanitizeError(err))
	}
	if length != object.Length || !bytes.Equal(digest, object.Digest) {
		return Object{}, ErrDigestMismatch
	}
	ready, err := s.store.ConfirmPayloadReady(ctx, object.ObjectID, length, digest, s.now().UTC())
	if errors.Is(err, store.ErrNotFound) {
		return Object{}, ErrExpired
	}
	if err != nil {
		return Object{}, err
	}
	s.observe(ctx)
	return ready, nil
}

// Open returns a lease-backed reader for one ready object. The lease is
// released exactly once on Close, so a retry or double close cannot
// double-release the charge.
func (s *Service) Open(ctx context.Context, ref Ref) (io.ReadCloser, Object, error) {
	if !s.Enabled() {
		return nil, Object{}, ErrDisabled
	}
	if err := validateRef(ref); err != nil {
		return nil, Object{}, err
	}
	now := s.now().UTC()
	current, err := s.store.GetPayload(ctx, ref.ObjectID)
	if err != nil {
		if errors.Is(err, store.ErrNotFound) {
			return nil, Object{}, ErrNotFound
		}
		return nil, Object{}, err
	}
	if !refMatches(ref, current) {
		return nil, Object{}, ErrForbidden
	}
	if current.ReaderCount >= MaxReadersPerObject {
		return nil, Object{}, fmt.Errorf("%w: too many readers", ErrCapacity)
	}
	leaseID, err := randomID()
	if err != nil {
		return nil, Object{}, err
	}
	object, err := s.store.OpenPayloadRead(ctx, s.readScope(ref), leaseID, now)
	if err != nil {
		switch {
		case errors.Is(err, store.ErrPayloadScopeMismatch):
			return nil, Object{}, ErrForbidden
		case errors.Is(err, store.ErrNotFound):
			return nil, Object{}, s.classifyUnavailable(ctx, ref.ObjectID, now)
		default:
			return nil, Object{}, err
		}
	}
	file, err := os.Open(s.finalPath(object.ObjectID))
	if err != nil {
		s.releaseLease(object.ObjectID, leaseID)
		s.closeObject(ctx, object.ObjectID)
		return nil, Object{}, ErrNotFound
	}
	info, err := file.Stat()
	if err != nil || info.Size() != object.Length {
		_ = file.Close()
		s.releaseLease(object.ObjectID, leaseID)
		s.closeObject(ctx, object.ObjectID)
		return nil, Object{}, ErrNotFound
	}
	reader := &leaseReader{
		File:      file,
		deadline:  time.Now().Add(s.config.ReadTimeout),
		expiresAt: object.ExpiresAt,
		now:       s.now,
		cancelled: func() bool { return s.isCancelled(object.ObjectID) },
	}
	reader.release = func() { s.releaseLease(object.ObjectID, leaseID) }
	return reader, object, nil
}

func (s *Service) classifyUnavailable(ctx context.Context, objectID string, now time.Time) error {
	object, err := s.store.GetPayload(ctx, objectID)
	if err != nil {
		if errors.Is(err, store.ErrNotFound) {
			return ErrNotFound
		}
		return err
	}
	switch object.State {
	case store.PayloadExpired, store.PayloadDeleted:
		return ErrExpired
	case store.PayloadReserved:
		return ErrNotReady
	default:
		if !object.ExpiresAt.After(now) {
			return ErrExpired
		}
		return ErrNotFound
	}
}

// Recover repairs the quota directory and metadata after restart in bounded
// batches: elapsed objects close, stale reservations publish a verified
// committed file or are abandoned, missing files fence ready rows, and files
// without live metadata are removed.
func (s *Service) Recover(ctx context.Context) error {
	if !s.Enabled() {
		return nil
	}
	now := s.now().UTC()
	batch := s.config.CleanupBatch
	var errs []error
	expired, err := s.store.ExpirePayloads(ctx, now, batch)
	if err != nil {
		errs = append(errs, err)
	}
	repairs := len(expired)
	stale, err := s.store.StaleReservedPayloads(ctx, now.Add(-s.config.UploadTimeout), batch)
	if err != nil {
		errs = append(errs, err)
	}
	for _, object := range stale {
		if _, statErr := os.Stat(s.finalPath(object.ObjectID)); statErr == nil {
			if _, publishErr := s.publishCommitted(ctx, object); publishErr == nil {
				repairs++
				continue
			}
		}
		if _, cancelErr := s.store.CancelPayload(ctx, store.PayloadReadScope{ObjectID: object.ObjectID}, now); cancelErr != nil {
			errs = append(errs, cancelErr)
		} else {
			repairs++
		}
	}
	page, err := s.store.ReadyPayloadPage(ctx, "", batch)
	if err != nil {
		errs = append(errs, err)
	}
	for _, object := range page {
		if _, statErr := os.Stat(s.finalPath(object.ObjectID)); errors.Is(statErr, os.ErrNotExist) {
			if _, cancelErr := s.store.CancelPayload(ctx, store.PayloadReadScope{ObjectID: object.ObjectID}, now); cancelErr != nil {
				errs = append(errs, cancelErr)
			} else {
				repairs++
			}
		}
	}
	files, err := s.scanManagedFiles(batch * 4)
	if err != nil {
		errs = append(errs, err)
	}
	orphans := 0
	for _, file := range files {
		object, getErr := s.store.GetPayload(ctx, file.ObjectID)
		switch {
		case errors.Is(getErr, store.ErrNotFound):
			if removeErr := os.Remove(file.Path); removeErr != nil && !errors.Is(removeErr, os.ErrNotExist) {
				errs = append(errs, sanitizeError(removeErr))
				continue
			}
			orphans++
		case getErr != nil:
			errs = append(errs, getErr)
		case object.State == store.PayloadDeleted:
			if removeErr := os.Remove(file.Path); removeErr != nil && !errors.Is(removeErr, os.ErrNotExist) {
				errs = append(errs, sanitizeError(removeErr))
				continue
			}
			orphans++
		}
	}
	if s.metrics != nil {
		s.metrics.PayloadReconcile("repairs", repairs)
		s.metrics.PayloadReconcile("orphans", orphans)
	}
	s.observe(ctx)
	if s.logger != nil && (repairs > 0 || orphans > 0) {
		s.logger.Info("payload storage reconciled", "repairs", repairs, "orphans", orphans)
	}
	return errors.Join(errs...)
}

// Cleanup expires elapsed objects, deletes files with no active reader,
// releases capacity once, and retries failed deletions with a bounded backoff.
func (s *Service) Cleanup(ctx context.Context) (CleanupResult, error) {
	result := CleanupResult{}
	if !s.Enabled() {
		return result, ErrDisabled
	}
	now := s.now().UTC()
	expired, err := s.store.ExpirePayloads(ctx, now, s.config.CleanupBatch)
	if err != nil {
		return result, err
	}
	result.Expired = len(expired)
	claimed, err := s.store.ClaimPayloadCleanup(ctx, now, s.config.CleanupBatch, s.cleanupLease())
	if err != nil {
		return result, err
	}
	var errs []error
	for _, object := range claimed {
		if err := s.removeObjectFiles(object.ObjectID); err != nil {
			result.Failed++
			retry := s.cleanupRetry(object.CleanupTries)
			if markErr := s.store.MarkPayloadCleanupFailure(ctx, object.ObjectID, s.now().UTC(), retry); markErr != nil {
				errs = append(errs, markErr)
			}
			errs = append(errs, err)
			continue
		}
		deleted, err := s.store.MarkPayloadDeleted(ctx, object.ObjectID, s.now().UTC())
		if err != nil {
			errs = append(errs, err)
			continue
		}
		if deleted {
			result.Deleted++
			result.ReclaimedBytes += object.Reserved
		}
	}
	tombstones, err := s.store.DeletePayloadTombstones(ctx, now.Add(-s.config.Retention), s.config.CleanupBatch)
	if err != nil {
		errs = append(errs, err)
	} else {
		result.Tombstones = tombstones
	}
	if s.metrics != nil {
		s.metrics.PayloadCleanup(result.Deleted, result.ReclaimedBytes, result.Failed)
	}
	s.observe(ctx)
	if s.logger != nil && (result.Expired > 0 || result.Deleted > 0 || result.Failed > 0) {
		s.logger.Info("payload cleanup pass",
			"expired", result.Expired,
			"deleted", result.Deleted,
			"reclaimed_bytes", result.ReclaimedBytes,
			"failed", result.Failed)
	}
	return result, errors.Join(errs...)
}

func (s *Service) cleanupLease() time.Duration {
	return 2 * s.config.CleanupInterval
}

func (s *Service) cleanupRetry(attempts int) time.Duration {
	retry := s.config.CleanupInterval
	for i := 0; i < attempts && retry < s.config.Retention; i++ {
		retry *= 2
	}
	if retry > s.config.Retention {
		retry = s.config.Retention
	}
	return retry
}

// removeObjectFiles deletes the committed file and any staged temp files for
// one object. A missing file is success; any other filesystem error keeps the
// charge in place for a retry.
func (s *Service) removeObjectFiles(objectID string) error {
	s.discardStaged(objectID)
	defer s.clearCancelled(objectID)
	finalPath := s.finalPath(objectID)
	if err := os.Remove(finalPath); err != nil && !errors.Is(err, os.ErrNotExist) {
		return sanitizeError(err)
	}
	entries, err := os.ReadDir(s.tmp)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return nil
		}
		return sanitizeError(err)
	}
	prefix := objectID + "."
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasPrefix(entry.Name(), prefix) {
			continue
		}
		if _, ok := matchTemporaryName(entry.Name()); !ok {
			continue
		}
		if err := os.Remove(filepath.Join(s.tmp, entry.Name())); err != nil && !errors.Is(err, os.ErrNotExist) {
			return sanitizeError(err)
		}
	}
	return nil
}

func (s *Service) closeObject(ctx context.Context, objectID string) {
	if _, err := s.store.CancelPayload(ctx, store.PayloadReadScope{ObjectID: objectID}, s.now().UTC()); err != nil {
		s.logger.Warn("payload close failed", "error", sanitizeError(err).Error())
	}
	s.discardStaged(objectID)
}

func (s *Service) releaseLease(objectID, leaseID string) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := s.store.ReleasePayloadRead(ctx, objectID, leaseID); err != nil {
		s.logger.Warn("payload reader lease release failed", "error", sanitizeError(err).Error())
	}
}

// refMatches compares the authorized scope with one stored object. An empty
// expected field is not checked. An empty *stored* AppSession is also treated
// as unbound: Host-originated uploads do not know the Access-managed session,
// so they are bound by Host, Device, channel, grant and the sealed channel
// instead. A session-bound reader of the same Device may locate such an object
// but still cannot decrypt ciphertext sealed to another Noise channel.
func refMatches(ref Ref, object Object) bool {
	checks := []struct{ expected, actual string }{
		{ref.HostID, object.HostID},
		{ref.DeviceID, object.DeviceID},
		{ref.AppSession, object.AppSession},
		{ref.ChannelID, object.ChannelID},
		{ref.Purpose, object.Purpose},
	}
	for _, check := range checks {
		if check.expected != "" && check.actual != "" && check.expected != check.actual {
			return false
		}
	}
	return true
}

func fileDigest(path string) (int64, []byte, error) {
	file, err := os.Open(path)
	if err != nil {
		return 0, nil, err
	}
	defer func() { _ = file.Close() }()
	hasher := sha256.New()
	length, err := io.Copy(hasher, file)
	if err != nil {
		return 0, nil, err
	}
	return length, hasher.Sum(nil), nil
}

func syncDirectory(path string) error {
	dir, err := os.Open(path)
	if err != nil {
		return err
	}
	defer func() { _ = dir.Close() }()
	return dir.Sync()
}

type leaseReader struct {
	*os.File
	deadline  time.Time
	expiresAt time.Time
	now       func() time.Time
	cancelled func() bool
	once      sync.Once
	release   func()
}

// Read enforces the configured read budget, the object's fixed expiry and any
// cancellation of its admission. An already-open reader therefore stops near
// the expiry or revocation instead of serving its whole lease.
func (r *leaseReader) Read(buffer []byte) (int, error) {
	if !r.deadline.IsZero() && time.Now().After(r.deadline) {
		return 0, os.ErrDeadlineExceeded
	}
	if !r.expiresAt.IsZero() && r.now != nil && !r.now().Before(r.expiresAt) {
		return 0, os.ErrDeadlineExceeded
	}
	if r.cancelled != nil && r.cancelled() {
		return 0, os.ErrDeadlineExceeded
	}
	return r.File.Read(buffer)
}

func (r *leaseReader) Close() error {
	err := r.File.Close()
	r.once.Do(func() {
		if r.release != nil {
			r.release()
		}
	})
	return err
}

type contextReader struct {
	ctx    context.Context
	reader io.Reader
}

func (r contextReader) Read(buffer []byte) (int, error) {
	select {
	case <-r.ctx.Done():
		return 0, r.ctx.Err()
	default:
	}
	return r.reader.Read(buffer)
}
