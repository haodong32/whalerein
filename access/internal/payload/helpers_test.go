//go:build unix

package payload

import (
	"context"
	"crypto/sha256"
	"errors"
	"log/slog"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/store"
)

// fakeStore scripts the SQL seam. Every hook models one store outcome; a test
// that reaches an unexpected statement fails instead of touching PostgreSQL.
type fakeStore struct {
	t *testing.T

	reserveFn   func(context.Context, store.PayloadReservation, store.PayloadLimits) (store.PayloadAdmission, error)
	getFn       func(context.Context, string) (store.PayloadObject, error)
	confirmFn   func(context.Context, string, int64, []byte, time.Time) (store.PayloadObject, error)
	openFn      func(context.Context, store.PayloadReadScope, string, time.Time) (store.PayloadObject, error)
	releaseFn   func(context.Context, string, string) error
	cancelFn    func(context.Context, store.PayloadReadScope, time.Time) (store.PayloadObject, error)
	expireFn    func(context.Context, time.Time, int) ([]store.PayloadObject, error)
	claimFn     func(context.Context, time.Time, int, time.Duration) ([]store.PayloadObject, error)
	failureFn   func(context.Context, string, time.Time, time.Duration) error
	deletedFn   func(context.Context, string, time.Time) (bool, error)
	tombstoneFn func(context.Context, time.Time, int) (int64, error)
	staleFn     func(context.Context, time.Time, int) ([]store.PayloadObject, error)
	readyPageFn func(context.Context, string, int) ([]store.PayloadObject, error)
	statsFn     func(context.Context, time.Time) (store.PayloadStats, error)
}

func (f *fakeStore) ReservePayload(ctx context.Context, reservation store.PayloadReservation, limits store.PayloadLimits) (store.PayloadAdmission, error) {
	if f.reserveFn == nil {
		f.t.Fatal("unexpected ReservePayload")
	}
	return f.reserveFn(ctx, reservation, limits)
}

func (f *fakeStore) GetPayload(ctx context.Context, id string) (store.PayloadObject, error) {
	if f.getFn == nil {
		f.t.Fatal("unexpected GetPayload")
	}
	return f.getFn(ctx, id)
}

func (f *fakeStore) ConfirmPayloadReady(ctx context.Context, id string, length int64, digest []byte, now time.Time) (store.PayloadObject, error) {
	if f.confirmFn == nil {
		f.t.Fatal("unexpected ConfirmPayloadReady")
	}
	return f.confirmFn(ctx, id, length, digest, now)
}

func (f *fakeStore) OpenPayloadRead(ctx context.Context, scope store.PayloadReadScope, lease string, now time.Time) (store.PayloadObject, error) {
	if f.openFn == nil {
		f.t.Fatal("unexpected OpenPayloadRead")
	}
	return f.openFn(ctx, scope, lease, now)
}

func (f *fakeStore) ReleasePayloadRead(ctx context.Context, objectID, lease string) error {
	if f.releaseFn == nil {
		f.t.Fatal("unexpected ReleasePayloadRead")
	}
	return f.releaseFn(ctx, objectID, lease)
}

func (f *fakeStore) CancelPayload(ctx context.Context, scope store.PayloadReadScope, now time.Time) (store.PayloadObject, error) {
	if f.cancelFn == nil {
		f.t.Fatal("unexpected CancelPayload")
	}
	return f.cancelFn(ctx, scope, now)
}

func (f *fakeStore) ExpirePayloads(ctx context.Context, now time.Time, limit int) ([]store.PayloadObject, error) {
	if f.expireFn == nil {
		f.t.Fatal("unexpected ExpirePayloads")
	}
	return f.expireFn(ctx, now, limit)
}

func (f *fakeStore) ClaimPayloadCleanup(ctx context.Context, now time.Time, limit int, lease time.Duration) ([]store.PayloadObject, error) {
	if f.claimFn == nil {
		f.t.Fatal("unexpected ClaimPayloadCleanup")
	}
	return f.claimFn(ctx, now, limit, lease)
}

func (f *fakeStore) MarkPayloadCleanupFailure(ctx context.Context, id string, now time.Time, retry time.Duration) error {
	if f.failureFn == nil {
		f.t.Fatal("unexpected MarkPayloadCleanupFailure")
	}
	return f.failureFn(ctx, id, now, retry)
}

func (f *fakeStore) MarkPayloadDeleted(ctx context.Context, id string, now time.Time) (bool, error) {
	if f.deletedFn == nil {
		f.t.Fatal("unexpected MarkPayloadDeleted")
	}
	return f.deletedFn(ctx, id, now)
}

func (f *fakeStore) DeletePayloadTombstones(ctx context.Context, before time.Time, limit int) (int64, error) {
	if f.tombstoneFn == nil {
		f.t.Fatal("unexpected DeletePayloadTombstones")
	}
	return f.tombstoneFn(ctx, before, limit)
}

func (f *fakeStore) StaleReservedPayloads(ctx context.Context, before time.Time, limit int) ([]store.PayloadObject, error) {
	if f.staleFn == nil {
		f.t.Fatal("unexpected StaleReservedPayloads")
	}
	return f.staleFn(ctx, before, limit)
}

func (f *fakeStore) ReadyPayloadPage(ctx context.Context, after string, limit int) ([]store.PayloadObject, error) {
	if f.readyPageFn == nil {
		f.t.Fatal("unexpected ReadyPayloadPage")
	}
	return f.readyPageFn(ctx, after, limit)
}

func (f *fakeStore) PayloadStats(ctx context.Context, now time.Time) (store.PayloadStats, error) {
	if f.statsFn == nil {
		return store.PayloadStats{}, nil
	}
	return f.statsFn(ctx, now)
}

func testConfig(root string) Config {
	return Config{
		Enabled:               true,
		RootDir:               root,
		NodeID:                "node-1",
		MaxObjectBytes:        1 << 20,
		MaxHostBytes:          4 << 20,
		MaxNodeBytes:          8 << 20,
		MaxObjects:            10,
		MaxActiveUploads:      4,
		FreeSpaceReserveBytes: 1,
		Retention:             time.Hour,
		CleanupInterval:       time.Minute,
		CleanupBatch:          10,
		UploadTimeout:         time.Minute,
		ReadTimeout:           time.Minute,
	}
}

func newTestService(t *testing.T, db Store, config Config, now time.Time) *Service {
	t.Helper()
	if now.IsZero() {
		now = time.Unix(1_700_000_000, 0).UTC()
	}
	service, err := New(Dependencies{
		Store:   db,
		Config:  config,
		Metrics: diagnostics.NewMetrics(),
		Logger:  slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelError})),
		Now:     func() time.Time { return now },
	})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	if service.objects != filepath.Join(config.RootDir, "objects") {
		t.Fatalf("objects dir = %q", service.objects)
	}
	return service
}

func objectFixture(now time.Time) store.PayloadObject {
	return store.PayloadObject{
		ObjectID:  "0123456789abcdef0123456789abcdef",
		NodeID:    "node-1",
		OwnerID:   "node-1",
		HostID:    "host-1",
		DeviceID:  "device-1",
		ChannelID: "channel-1",
		RequestID: "request-1",
		Purpose:   "result",
		State:     store.PayloadReserved,
		Reserved:  4,
		Length:    4,
		Digest:    digestOf([]byte("body")),
		ExpiresAt: now.Add(time.Hour),
		CreatedAt: now,
		UpdatedAt: now,
	}
}

func mustWriteFile(t *testing.T, path string, data []byte) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o750); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, data, 0o600); err != nil {
		t.Fatal(err)
	}
}

func requireError(t *testing.T, err, target error) {
	t.Helper()
	if !errors.Is(err, target) {
		t.Fatalf("error = %v, want %v", err, target)
	}
}

func digestOf(data []byte) []byte {
	sum := sha256.Sum256(data)
	return sum[:]
}
