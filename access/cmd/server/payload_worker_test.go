package main

import (
	"bytes"
	"context"
	"io"
	"log/slog"
	"os"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/config"
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
func (payloadWorkerStore) ExpirePayloads(context.Context, string, time.Time, int) ([]store.PayloadObject, error) {
	return nil, nil
}
func (payloadWorkerStore) ClaimPayloadCleanup(context.Context, string, time.Time, int, time.Duration) ([]store.PayloadObject, error) {
	return nil, nil
}
func (payloadWorkerStore) MarkPayloadCleanupFailure(context.Context, string, time.Time, time.Duration) error {
	return nil
}
func (payloadWorkerStore) MarkPayloadDeleted(context.Context, string, time.Time) (bool, error) {
	return false, nil
}
func (payloadWorkerStore) DeletePayloadTombstones(context.Context, string, time.Time, int) (int64, error) {
	return 0, nil
}
func (payloadWorkerStore) StaleReservedPayloads(context.Context, string, time.Time, int) ([]store.PayloadObject, error) {
	return nil, nil
}
func (payloadWorkerStore) ReadyPayloadPage(context.Context, string, string, int) ([]store.PayloadObject, error) {
	return nil, nil
}
func (payloadWorkerStore) PayloadStats(context.Context, string, time.Time) (store.PayloadStats, error) {
	return store.PayloadStats{}, nil
}

func TestPayloadConfigAndOwnerResolution(t *testing.T) {
	cfg := config.Default()
	cfg.Storage.Enabled = true
	cfg.Storage.RootDir = t.TempDir()
	cfg.Storage.NodeID = "node"
	got := payloadConfig(cfg)
	if !got.Enabled || got.NodeID != "node" || got.RootDir != cfg.Storage.RootDir {
		t.Fatalf("payload config=%+v", got)
	}
	cfg.Storage.OwnerID = "owner"
	if ownerNodeID(cfg) != "owner" {
		t.Fatalf("owner=%q", ownerNodeID(cfg))
	}
	cfg.Storage.OwnerID = ""
	if ownerNodeID(cfg) != "node" {
		t.Fatalf("node fallback=%q", ownerNodeID(cfg))
	}
}

func TestRunServerWithStoreRejectsConfigurationBeforeDatabase(t *testing.T) {
	logger := slog.New(slog.NewTextHandler(io.Discard, nil))
	for name, mutate := range map[string]func(*config.Config){
		"trusted proxy": func(c *config.Config) { c.HTTP.ClientAddress.TrustedProxies = []string{"not-a-prefix"} },
		"client reference key": func(c *config.Config) {
			c.HTTP.ClientReference.Key = "short"
			c.HTTP.ClientReference.Window = "1m"
		},
		"transport profile": func(c *config.Config) { c.Transport.Profile = "missing-profile" },
	} {
		t.Run(name, func(t *testing.T) {
			cfg := config.Default()
			mutate(&cfg)
			ops := startupStoreOps{
				open:    func(context.Context, string) (*store.Store, error) { t.Fatal("database opened"); return nil, nil },
				migrate: func(context.Context, *store.Store) error { t.Fatal("migration ran"); return nil },
				close:   func(*store.Store) {},
			}
			if err := runServerWithStore(context.Background(), cfg, logger, ops); err == nil {
				t.Fatal("invalid configuration accepted")
			}
		})
	}
}

func TestRunWithRejectsInvalidConfigurationAndArguments(t *testing.T) {
	var stdout, stderr bytes.Buffer
	if code := runWith(context.Background(), []string{"--unknown"}, &stdout, &stderr, startupStoreOps{}, func(context.Context, config.Config, *slog.Logger) error { return nil }); code != 2 {
		t.Fatalf("bad argument exit=%d", code)
	}
	if code := runWith(context.Background(), []string{"--config", t.TempDir() + "/missing.yaml"}, &stdout, &stderr, startupStoreOps{}, func(context.Context, config.Config, *slog.Logger) error { return nil }); code != 2 {
		t.Fatalf("invalid configuration exit=%d", code)
	}
}

func TestRunWithReportsMigrationAndServeFailures(t *testing.T) {
	dir := t.TempDir()
	path := dir + "/config.yaml"
	body := `
listen: "127.0.0.1:1"
internal_listen: "127.0.0.1:2"
public_url: "https://access.example.com"
database_path: "postgres://127.0.0.1:1/db"
admin_token: "a"
host_provisioning_token: "b"
broker:
  url: "tcp://127.0.0.1:1883"
  username: "access"
  password: "c"
internal:
  callback_token: "d"
http:
  client_reference:
    key: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
    window: "daily"
`
	if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
	var stdout, stderr bytes.Buffer
	failingOpen := startupStoreOps{
		open:    func(context.Context, string) (*store.Store, error) { return nil, context.DeadlineExceeded },
		migrate: func(context.Context, *store.Store) error { return nil },
		close:   func(*store.Store) {},
	}
	stdout.Reset()
	stderr.Reset()
	if code := runWith(context.Background(), []string{"-config", path, "-migrate-only"}, &stdout, &stderr, failingOpen, func(context.Context, config.Config, *slog.Logger) error { return nil }); code != 1 {
		t.Fatalf("open failure exit=%d stderr=%s", code, stderr.String())
	}
	failingMigrate := failingOpen
	failingMigrate.open = func(context.Context, string) (*store.Store, error) { return &store.Store{}, nil }
	failingMigrate.migrate = func(context.Context, *store.Store) error { return context.DeadlineExceeded }
	if code := runWith(context.Background(), []string{"-config", path, "-migrate-only"}, &stdout, &stderr, failingMigrate, func(context.Context, config.Config, *slog.Logger) error { return nil }); code != 1 {
		t.Fatalf("migrate failure exit=%d", code)
	}
	if code := runWith(context.Background(), []string{"-config", path}, &stdout, &stderr, failingMigrate, func(context.Context, config.Config, *slog.Logger) error { return context.DeadlineExceeded }); code != 1 {
		t.Fatalf("serve failure exit=%d", code)
	}
}

func TestRunServerWithStoreRejectsStorageAndBrokerConfiguration(t *testing.T) {
	logger := slog.New(slog.NewTextHandler(io.Discard, nil))
	ops := startupStoreOps{
		open:    func(context.Context, string) (*store.Store, error) { return &store.Store{}, nil },
		migrate: func(context.Context, *store.Store) error { return nil },
		close:   func(*store.Store) {},
	}
	invalidStorage := config.Default()
	invalidStorage.Storage.Enabled = true
	invalidStorage.Storage.RootDir = "relative/payloads"
	if err := runServerWithStore(context.Background(), invalidStorage, logger, ops); err == nil {
		t.Fatal("relative storage root accepted")
	}
	invalidBroker := config.Default()
	invalidBroker.Broker.URL = "http://127.0.0.1:1"
	if err := runServerWithStore(context.Background(), invalidBroker, logger, ops); err == nil {
		t.Fatal("unsupported broker scheme accepted")
	}
}
