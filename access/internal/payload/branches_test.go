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

func TestPublishCommittedRejectsUnreadableFile(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	object := objectFixture(now)
	db := &fakeStore{t: t, getFn: func(context.Context, string) (store.PayloadObject, error) { return object, nil }}
	service := newTestService(t, db, testConfig(t.TempDir()), now)
	// A directory at the final path opens successfully but cannot be read as a
	// ciphertext file.
	if err := os.MkdirAll(service.finalPath(object.ObjectID), 0o750); err != nil {
		t.Fatal(err)
	}
	_, err := service.Finish(context.Background(), Ref{ObjectID: object.ObjectID})
	if err == nil {
		t.Fatal("finish accepted an unreadable committed file")
	}
}

func TestFinishRenameFailure(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	db := &fakeStore{t: t}
	service := newTestService(t, db, testConfig(t.TempDir()), now)
	object := stagedObject(t, service, db, []byte("body"), now)
	// A non-empty directory at the destination makes the atomic rename fail.
	final := service.finalPath(object.ObjectID)
	if err := os.MkdirAll(final, 0o750); err != nil {
		t.Fatal(err)
	}
	mustWriteFile(t, filepath.Join(final, "child"), []byte("x"))
	_, err := service.Finish(context.Background(), Ref{ObjectID: object.ObjectID})
	if err == nil {
		t.Fatal("finish accepted a failed rename")
	}
}

func TestClassifyUnavailableDeletedAndReadyStates(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	ready := objectFixture(now)
	ready.State = store.PayloadReady
	for _, tc := range []struct {
		name   string
		object store.PayloadObject
		want   error
	}{
		{"deleted", func() store.PayloadObject { o := ready; o.State = store.PayloadDeleted; return o }(), ErrExpired},
		{"ready future", ready, ErrNotFound},
	} {
		t.Run(tc.name, func(t *testing.T) {
			calls := 0
			db := &fakeStore{t: t,
				getFn: func(context.Context, string) (store.PayloadObject, error) {
					calls++
					if calls == 1 {
						return ready, nil
					}
					return tc.object, nil
				},
				openFn: func(context.Context, store.PayloadReadScope, string, time.Time) (store.PayloadObject, error) {
					return store.PayloadObject{}, store.ErrNotFound
				},
			}
			service := newTestService(t, db, testConfig(t.TempDir()), now)
			_, _, err := service.Open(context.Background(), Ref{ObjectID: ready.ObjectID})
			requireError(t, err, tc.want)
		})
	}
}

func TestCancelRejectsInvalidAndGenericFailure(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	t.Run("invalid", func(t *testing.T) {
		service := newTestService(t, &fakeStore{t: t}, testConfig(t.TempDir()), now)
		requireError(t, service.Cancel(context.Background(), Ref{ObjectID: "bad"}), ErrInvalid)
	})
	t.Run("generic", func(t *testing.T) {
		db := &fakeStore{t: t, cancelFn: func(context.Context, store.PayloadReadScope, time.Time) (store.PayloadObject, error) {
			return store.PayloadObject{}, errors.New("db down")
		}}
		service := newTestService(t, db, testConfig(t.TempDir()), now)
		if err := service.Cancel(context.Background(), Ref{ObjectID: objectFixture(now).ObjectID}); err == nil {
			t.Fatal("cancel hid a store failure")
		}
	})
}

func TestStatsFailsWhenFilesystemIsUnavailable(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	config := testConfig(t.TempDir())
	service := newTestService(t, &fakeStore{t: t}, config, now)
	if err := os.RemoveAll(config.RootDir); err != nil {
		t.Fatal(err)
	}
	if _, err := service.Stats(context.Background()); err == nil {
		t.Fatal("stats hid a missing filesystem")
	}
}

func TestServiceWithoutMetricsAndFailingObservation(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	config := testConfig(t.TempDir())
	service, err := New(Dependencies{Store: &fakeStore{t: t}, Config: config, Now: func() time.Time { return now }})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := service.Stats(context.Background()); err != nil {
		t.Fatalf("stats without metrics: %v", err)
	}
	db := &fakeStore{t: t, statsFn: func(context.Context, time.Time) (store.PayloadStats, error) {
		return store.PayloadStats{}, errors.New("stats")
	}}
	service = newTestService(t, db, config, now)
	service.observe(context.Background())
}

func TestCleanupReportsMetadataAndTombstoneFailures(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	object := objectFixture(now)
	object.State = store.PayloadExpired
	db := &fakeStore{t: t,
		expireFn: func(context.Context, time.Time, int) ([]store.PayloadObject, error) { return nil, nil },
		claimFn: func(context.Context, time.Time, int, time.Duration) ([]store.PayloadObject, error) {
			return []store.PayloadObject{object}, nil
		},
		deletedFn:   func(context.Context, string, time.Time) (bool, error) { return false, errors.New("delete") },
		tombstoneFn: func(context.Context, time.Time, int) (int64, error) { return 0, errors.New("tombstone") },
	}
	service := newTestService(t, db, testConfig(t.TempDir()), now)
	if _, err := service.Cleanup(context.Background()); err == nil {
		t.Fatal("cleanup hid metadata failures")
	}
}

func TestRecoverHandlesPublishFailureAndStoreReadErrors(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	stale := objectFixture(now)
	stale.ObjectID = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
	broken := objectFixture(now)
	broken.ObjectID = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
	cancelled := 0
	db := &fakeStore{t: t,
		expireFn: func(context.Context, time.Time, int) ([]store.PayloadObject, error) { return nil, nil },
		staleFn: func(context.Context, time.Time, int) ([]store.PayloadObject, error) {
			return []store.PayloadObject{stale}, nil
		},
		readyPageFn: func(context.Context, string, int) ([]store.PayloadObject, error) {
			return []store.PayloadObject{broken}, nil
		},
		getFn: func(_ context.Context, id string) (store.PayloadObject, error) {
			if id == broken.ObjectID {
				return store.PayloadObject{}, errors.New("read")
			}
			return store.PayloadObject{ObjectID: id, State: store.PayloadDeleted}, nil
		},
		cancelFn: func(context.Context, store.PayloadReadScope, time.Time) (store.PayloadObject, error) {
			cancelled++
			return store.PayloadObject{}, nil
		},
	}
	service := newTestService(t, db, testConfig(t.TempDir()), now)
	// The stale file does not match the reserved digest, so reconciliation
	// abandons the reservation instead of publishing it.
	mustWriteFile(t, service.finalPath(stale.ObjectID), []byte("evil"))
	// An unreadable object root also fails the bounded directory scan.
	if err := os.RemoveAll(service.objects); err != nil {
		t.Fatal(err)
	}
	mustWriteFile(t, service.objects, []byte("not a directory"))
	err := service.Recover(context.Background())
	if err == nil {
		t.Fatal("recover hid store and directory failures")
	}
	if cancelled != 1 {
		t.Fatalf("cancelled = %d", cancelled)
	}
}

func TestRemoveObjectFilesSkipsUnrelatedEntries(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	object := objectFixture(now)
	object.State = store.PayloadExpired
	db := &fakeStore{t: t,
		expireFn: func(context.Context, time.Time, int) ([]store.PayloadObject, error) { return nil, nil },
		claimFn: func(context.Context, time.Time, int, time.Duration) ([]store.PayloadObject, error) {
			return []store.PayloadObject{object}, nil
		},
		deletedFn:   func(context.Context, string, time.Time) (bool, error) { return true, nil },
		tombstoneFn: func(context.Context, time.Time, int) (int64, error) { return 0, nil },
	}
	service := newTestService(t, db, testConfig(t.TempDir()), now)
	// A directory and a non-matching temp name share the object prefix but must
	// not be removed.
	if err := os.MkdirAll(filepath.Join(service.tmp, object.ObjectID+".suffix.tmp"), 0o750); err != nil {
		t.Fatal(err)
	}
	mustWriteFile(t, filepath.Join(service.tmp, object.ObjectID+".short.tmp"), []byte("x"))
	if _, err := service.Cleanup(context.Background()); err != nil {
		t.Fatalf("Cleanup: %v", err)
	}
	if _, err := os.Stat(filepath.Join(service.tmp, object.ObjectID+".suffix.tmp")); err != nil {
		t.Fatal("cleanup removed an unrelated directory")
	}
	if _, err := os.Stat(filepath.Join(service.tmp, object.ObjectID+".short.tmp")); err != nil {
		t.Fatal("cleanup removed an unrelated temp name")
	}
}

func TestScanManagedFilesSkipsDirectories(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	service := newTestService(t, &fakeStore{t: t}, testConfig(t.TempDir()), now)
	id := "0123456789abcdef0123456789abcdef"
	if err := os.MkdirAll(filepath.Join(service.tmp, id+"."+strings.Repeat("a", 32)+".tmp"), 0o750); err != nil {
		t.Fatal(err)
	}
	if err := os.MkdirAll(filepath.Join(service.objects, id+".bin"), 0o750); err != nil {
		t.Fatal(err)
	}
	files, err := service.scanManagedFiles(10)
	if err != nil {
		t.Fatal(err)
	}
	if len(files) != 0 {
		t.Fatalf("directories were treated as files: %+v", files)
	}
}
