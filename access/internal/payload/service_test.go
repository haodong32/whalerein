//go:build unix

package payload

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
)

func TestConfigValidationRejectsUnsafeCapacity(t *testing.T) {
	root := t.TempDir()
	valid := testConfig(root)
	valid.OwnerID = "owner"
	if err := valid.Validate(); err != nil {
		t.Fatalf("valid config: %v", err)
	}
	if err := (Config{}).Validate(); err != nil {
		t.Fatalf("disabled config: %v", err)
	}
	cases := []struct {
		name   string
		mutate func(*Config)
		want   string
	}{
		{"relative root", func(c *Config) { c.RootDir = "relative/path" }, "root_dir"},
		{"missing node", func(c *Config) { c.NodeID = "" }, "node_id"},
		{"zero object", func(c *Config) { c.MaxObjectBytes = 0 }, "byte limits"},
		{"host below object", func(c *Config) { c.MaxHostBytes = c.MaxObjectBytes - 1 }, "max_host_bytes"},
		{"node below host", func(c *Config) { c.MaxNodeBytes = c.MaxHostBytes - 1 }, "max_node_bytes"},
		{"zero objects", func(c *Config) { c.MaxObjects = 0 }, "max_objects"},
		{"uploads above objects", func(c *Config) { c.MaxActiveUploads = c.MaxObjects + 1 }, "max_active_uploads"},
		{"zero reserve", func(c *Config) { c.FreeSpaceReserveBytes = 0 }, "free_space_reserve"},
		{"zero retention", func(c *Config) { c.Retention = 0 }, "retention"},
		{"zero cleanup", func(c *Config) { c.CleanupInterval = 0 }, "cleanup_interval"},
		{"zero batch", func(c *Config) { c.CleanupBatch = 0 }, "cleanup_batch"},
		{"zero upload timeout", func(c *Config) { c.UploadTimeout = 0 }, "upload_timeout"},
		{"zero read timeout", func(c *Config) { c.ReadTimeout = 0 }, "read_timeout"},
		{"upload beyond retention", func(c *Config) { c.UploadTimeout = c.Retention + time.Second }, "upload_timeout must not exceed"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			config := testConfig(root)
			tc.mutate(&config)
			err := config.Validate()
			if err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("error = %v, want %q", err, tc.want)
			}
		})
	}
}

func TestNewRequiresStoreAndDisablesWithoutDirectory(t *testing.T) {
	if _, err := New(Dependencies{Config: testConfig(t.TempDir())}); err == nil {
		t.Fatal("a nil store was accepted")
	}
	service, err := New(Dependencies{Store: &fakeStore{t: t}, Config: Config{}})
	if err != nil {
		t.Fatalf("disabled New: %v", err)
	}
	if service.Enabled() {
		t.Fatal("zero config must be disabled")
	}
	if err := service.Ready(context.Background()); err != nil {
		t.Fatalf("disabled readiness: %v", err)
	}
	if _, err := service.Reserve(context.Background(), ReserveInput{}); !errors.Is(err, ErrDisabled) {
		t.Fatalf("disabled reserve = %v", err)
	}
	if _, err := service.Cleanup(context.Background()); !errors.Is(err, ErrDisabled) {
		t.Fatalf("disabled cleanup = %v", err)
	}
	if _, err := service.Stats(context.Background()); !errors.Is(err, ErrDisabled) {
		t.Fatalf("disabled stats = %v", err)
	}
	if err := service.Recover(context.Background()); err != nil {
		t.Fatalf("disabled recover: %v", err)
	}
	if err := service.Write(context.Background(), Ref{}, strings.NewReader("")); !errors.Is(err, ErrDisabled) {
		t.Fatalf("disabled write = %v", err)
	}
}

func TestNewRejectsUnwritableRoot(t *testing.T) {
	root := t.TempDir()
	blocked := filepath.Join(root, "file")
	mustWriteFile(t, blocked, []byte("x"))
	config := testConfig(blocked)
	if _, err := New(Dependencies{Store: &fakeStore{t: t}, Config: config}); err == nil {
		t.Fatal("a regular file as root was accepted")
	}
}

func TestReserveAppliesHardExpiryAndBounds(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	var captured store.PayloadReservation
	db := &fakeStore{
		t: t,
		statsFn: func(context.Context, time.Time) (store.PayloadStats, error) {
			return store.PayloadStats{}, nil
		},
		reserveFn: func(_ context.Context, reservation store.PayloadReservation, limits store.PayloadLimits) (store.PayloadAdmission, error) {
			captured = reservation
			if limits.MaxActiveUploads != 4 {
				t.Fatalf("limits = %+v", limits)
			}
			return store.PayloadAdmission{Object: store.PayloadObject{ObjectID: reservation.ObjectID, ExpiresAt: reservation.ExpiresAt}}, nil
		},
	}
	service := newTestService(t, db, testConfig(t.TempDir()), now)

	object, err := service.Reserve(context.Background(), ReserveInput{
		HostID:         "host-1",
		DeviceID:       "device-1",
		RequestID:      "request-1",
		Purpose:        "result",
		Length:         64,
		Digest:         digestOf([]byte("ciphertext")),
		ParentDeadline: now.Add(10 * time.Minute),
	})
	if err != nil {
		t.Fatalf("Reserve: %v", err)
	}
	if !objectIDPattern.MatchString(object.ObjectID) || len(object.ObjectID) != 32 {
		t.Fatalf("generated id = %q", object.ObjectID)
	}
	if !captured.ExpiresAt.Equal(now.Add(10 * time.Minute)) {
		t.Fatalf("expiry = %v, want parent deadline", captured.ExpiresAt)
	}
	if captured.ReservedBytes != 64 || captured.OwnerID != "node-1" {
		t.Fatalf("reservation = %+v", captured)
	}
}

func TestReserveTypedRefusals(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	base := ReserveInput{
		HostID: "host-1", RequestID: "request-1", Purpose: "result",
		Length: 16, Digest: digestOf([]byte("ciphertext")),
	}
	tests := []struct {
		name  string
		stats store.PayloadStats
		admit store.PayloadAdmission
		err   error
		want  error
	}{
		{name: "object size", err: nil, want: ErrTooLarge, admit: store.PayloadAdmission{Reason: store.PayloadRefusedSize}},
		{name: "host quota", want: ErrQuota, admit: store.PayloadAdmission{Reason: store.PayloadRefusedQuota}},
		{name: "node capacity", want: ErrCapacity, admit: store.PayloadAdmission{Reason: store.PayloadRefusedCapacity}},
		{name: "duplicate", want: ErrDuplicate, err: store.ErrPayloadDuplicate},
		{name: "headroom", want: ErrHeadroom, stats: store.PayloadStats{ReservedBytes: 1 << 60}},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			db := &fakeStore{
				t:       t,
				statsFn: func(context.Context, time.Time) (store.PayloadStats, error) { return tc.stats, nil },
			}
			if tc.err == nil && tc.stats.ReservedBytes == 0 {
				db.reserveFn = func(context.Context, store.PayloadReservation, store.PayloadLimits) (store.PayloadAdmission, error) {
					return tc.admit, nil
				}
			} else if tc.err != nil {
				db.reserveFn = func(context.Context, store.PayloadReservation, store.PayloadLimits) (store.PayloadAdmission, error) {
					return store.PayloadAdmission{}, tc.err
				}
			}
			service := newTestService(t, db, testConfig(t.TempDir()), now)
			_, err := service.Reserve(context.Background(), base)
			requireError(t, err, tc.want)
		})
	}
}

func TestReserveRejectsInvalidScopeBeforeStorage(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	db := &fakeStore{t: t}
	service := newTestService(t, db, testConfig(t.TempDir()), now)
	valid := ReserveInput{HostID: "host", RequestID: "request", Purpose: "result", Length: 4, Digest: digestOf([]byte("body"))}
	mutations := []struct {
		name   string
		mutate func(*ReserveInput)
		want   error
	}{
		{"bad object id", func(in *ReserveInput) { in.ObjectID = "ABC" }, ErrInvalid},
		{"missing host", func(in *ReserveInput) { in.HostID = "" }, ErrInvalid},
		{"missing purpose", func(in *ReserveInput) { in.Purpose = "" }, ErrInvalid},
		{"no parent", func(in *ReserveInput) { in.RequestID = "" }, ErrInvalid},
		{"two parents", func(in *ReserveInput) { in.MessageID = "message" }, ErrInvalid},
		{"session without device", func(in *ReserveInput) { in.AppSession = "session" }, ErrInvalid},
		{"zero length", func(in *ReserveInput) { in.Length = 0 }, ErrInvalid},
		{"reserved below length", func(in *ReserveInput) { in.ReservedBytes = 2 }, ErrInvalid},
		{"short digest", func(in *ReserveInput) { in.Digest = []byte{1} }, ErrInvalid},
		{"past deadline", func(in *ReserveInput) { in.ParentDeadline = now.Add(-time.Second) }, ErrExpired},
		{"above object limit", func(in *ReserveInput) { in.Length = 2 << 20 }, ErrTooLarge},
	}
	for _, tc := range mutations {
		t.Run(tc.name, func(t *testing.T) {
			input := valid
			input.Digest = append([]byte(nil), valid.Digest...)
			tc.mutate(&input)
			_, err := service.Reserve(context.Background(), input)
			requireError(t, err, tc.want)
		})
	}
}

func TestReserveHonorsDeadlineShorterThanRetention(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	var captured store.PayloadReservation
	db := &fakeStore{
		t:       t,
		statsFn: func(context.Context, time.Time) (store.PayloadStats, error) { return store.PayloadStats{}, nil },
		reserveFn: func(_ context.Context, reservation store.PayloadReservation, _ store.PayloadLimits) (store.PayloadAdmission, error) {
			captured = reservation
			return store.PayloadAdmission{Object: store.PayloadObject{ObjectID: reservation.ObjectID}}, nil
		},
	}
	config := testConfig(t.TempDir())
	config.Retention = 30 * time.Minute
	service := newTestService(t, db, config, now)
	if _, err := service.Reserve(context.Background(), ReserveInput{
		HostID: "host", RequestID: "request", Purpose: "result",
		Length: 4, Digest: digestOf([]byte("body")), ParentDeadline: now.Add(5 * time.Minute),
	}); err != nil {
		t.Fatalf("Reserve: %v", err)
	}
	if want := now.Add(5 * time.Minute); !captured.ExpiresAt.Equal(want) {
		t.Fatalf("expiry = %v, want %v", captured.ExpiresAt, want)
	}
}

func TestStatsComputesHeadroomFromReservedWrites(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	db := &fakeStore{
		t: t,
		statsFn: func(context.Context, time.Time) (store.PayloadStats, error) {
			return store.PayloadStats{
				ReservedObjects: 2, ReadyObjects: 3, ExpiredObjects: 1, DeletedObjects: 4,
				ChargedBytes: 100, ReservedBytes: 40, ReadyBytes: 60,
				ExpiredBytes: 5, ActiveUploads: 2, ReaderLeases: 1,
				OldestExpiredBacklog: 90,
			}, nil
		},
	}
	config := testConfig(t.TempDir())
	service := newTestService(t, db, config, now)
	stats, err := service.Stats(context.Background())
	if err != nil {
		t.Fatalf("Stats: %v", err)
	}
	if stats.ChargedBytes != 100 || stats.ReservedBytes != 40 || stats.ReaderLeases != 1 {
		t.Fatalf("stats = %+v", stats)
	}
	if stats.OldestExpiredBacklog != 90*time.Second {
		t.Fatalf("backlog = %v", stats.OldestExpiredBacklog)
	}
	if want := stats.FreeBytes - config.FreeSpaceReserveBytes - 40; stats.HeadroomBytes != want {
		t.Fatalf("headroom = %d, want %d (free = %d)", stats.HeadroomBytes, want, stats.FreeBytes)
	}
}

func TestStatsFailsWhenStorageIsUnavailable(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	db := &fakeStore{
		t: t,
		statsFn: func(context.Context, time.Time) (store.PayloadStats, error) {
			return store.PayloadStats{}, errors.New("db down")
		},
	}
	service := newTestService(t, db, testConfig(t.TempDir()), now)
	if _, err := service.Stats(context.Background()); err == nil {
		t.Fatal("stats ignored a storage failure")
	}
}

func TestReadyFailsWhenReserveExceedsFreeSpace(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	config := testConfig(t.TempDir())
	config.FreeSpaceReserveBytes = 1 << 60
	service, err := New(Dependencies{Store: &fakeStore{t: t}, Config: config})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	requireError(t, service.Ready(context.Background()), ErrHeadroom)
	_ = now
}

func TestReadyFailsWhenQuotaDirectoryIsRemoved(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	config := testConfig(t.TempDir())
	service := newTestService(t, &fakeStore{t: t}, config, now)
	if err := os.RemoveAll(service.tmp); err != nil {
		t.Fatal(err)
	}
	if err := service.Ready(context.Background()); err == nil {
		t.Fatal("missing quota directory was reported ready")
	}
}
