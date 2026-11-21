package relay

import (
	"context"
	"io"
	"log/slog"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/payload"
	"github.com/whalerein/whalerein/access/internal/store"
)

type payloadWorkerStore struct{}

func (payloadWorkerStore) ReservePayload(context.Context, store.PayloadReservation, store.PayloadLimits) (store.PayloadAdmission, error) {
	return store.PayloadAdmission{}, nil
}
func (payloadWorkerStore) GetPayload(context.Context, string) (store.PayloadObject, error) {
	return store.PayloadObject{}, store.ErrNotFound
}
func (payloadWorkerStore) ConfirmPayloadReady(context.Context, string, int64, []byte, time.Time) (store.PayloadObject, error) {
	return store.PayloadObject{}, store.ErrNotFound
}
func (payloadWorkerStore) OpenPayloadRead(context.Context, store.PayloadReadScope, string, time.Time) (store.PayloadObject, error) {
	return store.PayloadObject{}, store.ErrNotFound
}
func (payloadWorkerStore) ReleasePayloadRead(context.Context, string, string) error { return nil }
func (payloadWorkerStore) CancelPayload(context.Context, store.PayloadReadScope, time.Time) (store.PayloadObject, error) {
	return store.PayloadObject{}, store.ErrNotFound
}
func (payloadWorkerStore) ExpirePayloads(context.Context, time.Time, int) ([]store.PayloadObject, error) {
	return nil, nil
}
func (payloadWorkerStore) ClaimPayloadCleanup(context.Context, time.Time, int, time.Duration) ([]store.PayloadObject, error) {
	return nil, nil
}
func (payloadWorkerStore) MarkPayloadCleanupFailure(context.Context, string, time.Time, time.Duration) error {
	return nil
}
func (payloadWorkerStore) MarkPayloadDeleted(context.Context, string, time.Time) (bool, error) {
	return false, nil
}
func (payloadWorkerStore) DeletePayloadTombstones(context.Context, time.Time, int) (int64, error) {
	return 0, nil
}
func (payloadWorkerStore) StaleReservedPayloads(context.Context, time.Time, int) ([]store.PayloadObject, error) {
	return nil, nil
}
func (payloadWorkerStore) ReadyPayloadPage(context.Context, string, int) ([]store.PayloadObject, error) {
	return nil, nil
}
func (payloadWorkerStore) PayloadStats(context.Context, time.Time) (store.PayloadStats, error) {
	return store.PayloadStats{}, nil
}

func TestRunPayloadStorageCleansReconcilesAndStops(t *testing.T) {
	dir := t.TempDir()
	service, err := payload.New(payload.Dependencies{
		Store: payloadWorkerStore{}, Logger: slog.New(slog.NewTextHandler(io.Discard, nil)),
		Config: payload.Config{
			Enabled: true, RootDir: dir, NodeID: "node", MaxObjectBytes: 1 << 20,
			MaxHostBytes: 2 << 20, MaxNodeBytes: 4 << 20, MaxObjects: 4, MaxActiveUploads: 2,
			FreeSpaceReserveBytes: 1, Retention: time.Hour, CleanupInterval: time.Minute,
			CleanupBatch: 2, UploadTimeout: time.Minute, ReadTimeout: time.Minute,
		},
	})
	if err != nil {
		t.Fatal(err)
	}
	stager, err := NewStager(nil, nil, StagerConfig{})
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	go func() {
		defer close(done)
		RunPayloadStorage(ctx, service, stager, time.Millisecond, slog.New(slog.NewTextHandler(io.Discard, nil)))
	}()
	time.Sleep(30 * time.Millisecond)
	cancel()
	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatal("payload storage worker did not stop")
	}
}
