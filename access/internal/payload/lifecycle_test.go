//go:build unix

package payload

import (
	"bytes"
	"context"
	"errors"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
)

func stagedObject(t *testing.T, service *Service, db *fakeStore, body []byte, now time.Time) store.PayloadObject {
	t.Helper()
	object := objectFixture(now)
	object.Length = int64(len(body))
	object.Reserved = int64(len(body))
	object.Digest = digestOf(body)
	db.getFn = func(context.Context, string) (store.PayloadObject, error) { return object, nil }
	db.confirmFn = func(_ context.Context, id string, length int64, digest []byte, at time.Time) (store.PayloadObject, error) {
		if id != object.ObjectID || length != object.Length || !bytes.Equal(digest, object.Digest) {
			t.Fatalf("confirm id=%s length=%d digest=%x", id, length, digest)
		}
		ready := object
		ready.State = store.PayloadReady
		ready.ReadyAt = &at
		return ready, nil
	}
	if err := service.Write(context.Background(), Ref{ObjectID: object.ObjectID, HostID: object.HostID}, bytes.NewReader(body)); err != nil {
		t.Fatalf("Write: %v", err)
	}
	return object
}

func TestWriteAndFinishPublishOnlyAfterAtomicRename(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	db := &fakeStore{t: t}
	service := newTestService(t, db, testConfig(t.TempDir()), now)
	body := []byte("sealed-ciphertext")
	object := stagedObject(t, service, db, body, now)

	if path, ok := service.stagedPath(object.ObjectID); !ok || !strings.HasSuffix(path, ".tmp") {
		t.Fatalf("staged path = %q ok=%v", path, ok)
	}
	entries, err := os.ReadDir(service.tmp)
	if err != nil || len(entries) != 1 {
		t.Fatalf("tmp entries = %v err=%v", entries, err)
	}
	for _, entry := range entries {
		info, err := entry.Info()
		if err != nil || info.Mode().Perm() != 0o600 {
			t.Fatalf("staged mode = %v err=%v", info.Mode(), err)
		}
	}

	ready, err := service.Finish(context.Background(), Ref{ObjectID: object.ObjectID, HostID: object.HostID})
	if err != nil {
		t.Fatalf("Finish: %v", err)
	}
	if ready.State != store.PayloadReady {
		t.Fatalf("state = %q", ready.State)
	}
	if _, err := os.Stat(service.finalPath(object.ObjectID)); err != nil {
		t.Fatalf("final file: %v", err)
	}
	if entries, _ := os.ReadDir(service.tmp); len(entries) != 0 {
		t.Fatalf("staged temp survived finish: %v", entries)
	}
	if _, ok := service.stagedPath(object.ObjectID); ok {
		t.Fatal("staged entry survived finish")
	}
}

func TestWriteRejectsMismatchedLengthAndDigest(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	body := []byte("abcd")
	tests := []struct {
		name string
		body []byte
		want error
	}{
		{"longer", []byte("abcde"), ErrLengthMismatch},
		{"shorter", []byte("ab"), ErrLengthMismatch},
		{"same length wrong content", []byte("abce"), ErrDigestMismatch},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			db := &fakeStore{t: t, getFn: func(context.Context, string) (store.PayloadObject, error) {
				object := objectFixture(now)
				object.Length, object.Reserved, object.Digest = int64(len(body)), int64(len(body)), digestOf(body)
				return object, nil
			}}
			service := newTestService(t, db, testConfig(t.TempDir()), now)
			err := service.Write(context.Background(), Ref{ObjectID: objectFixture(now).ObjectID}, bytes.NewReader(tc.body))
			requireError(t, err, tc.want)
			if entries, _ := os.ReadDir(service.tmp); len(entries) != 0 {
				t.Fatalf("temp survived a rejected body: %v", entries)
			}
		})
	}
}

func TestWriteRejectsExpiredForeignAndReadyObjects(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	t.Run("expired", func(t *testing.T) {
		object := objectFixture(now)
		object.ExpiresAt = now.Add(-time.Second)
		cancelled := false
		db := &fakeStore{t: t,
			getFn: func(context.Context, string) (store.PayloadObject, error) { return object, nil },
			cancelFn: func(context.Context, store.PayloadReadScope, time.Time) (store.PayloadObject, error) {
				cancelled = true
				return object, nil
			},
		}
		service := newTestService(t, db, testConfig(t.TempDir()), now)
		requireError(t, service.Write(context.Background(), Ref{ObjectID: object.ObjectID}, strings.NewReader("body")), ErrExpired)
		if !cancelled {
			t.Fatal("expired upload did not close admission")
		}
	})
	t.Run("foreign scope", func(t *testing.T) {
		object := objectFixture(now)
		db := &fakeStore{t: t, getFn: func(context.Context, string) (store.PayloadObject, error) { return object, nil }}
		service := newTestService(t, db, testConfig(t.TempDir()), now)
		requireError(t, service.Write(context.Background(), Ref{ObjectID: object.ObjectID, HostID: "other"}, strings.NewReader("body")), ErrForbidden)
	})
	t.Run("not reserved", func(t *testing.T) {
		object := objectFixture(now)
		object.State = store.PayloadReady
		db := &fakeStore{t: t, getFn: func(context.Context, string) (store.PayloadObject, error) { return object, nil }}
		service := newTestService(t, db, testConfig(t.TempDir()), now)
		requireError(t, service.Write(context.Background(), Ref{ObjectID: object.ObjectID}, strings.NewReader("body")), ErrNotReady)
	})
	t.Run("missing", func(t *testing.T) {
		db := &fakeStore{t: t, getFn: func(context.Context, string) (store.PayloadObject, error) {
			return store.PayloadObject{}, store.ErrNotFound
		}}
		service := newTestService(t, db, testConfig(t.TempDir()), now)
		requireError(t, service.Write(context.Background(), Ref{ObjectID: objectFixture(now).ObjectID}, strings.NewReader("body")), ErrNotFound)
	})
}

func TestFinishIdempotentAndRecoveryPaths(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	t.Run("already ready", func(t *testing.T) {
		object := objectFixture(now)
		object.State = store.PayloadReady
		db := &fakeStore{t: t, getFn: func(context.Context, string) (store.PayloadObject, error) { return object, nil }}
		service := newTestService(t, db, testConfig(t.TempDir()), now)
		got, err := service.Finish(context.Background(), Ref{ObjectID: object.ObjectID})
		if err != nil || got.State != store.PayloadReady {
			t.Fatalf("finish ready = %+v err=%v", got, err)
		}
	})
	t.Run("expired", func(t *testing.T) {
		object := objectFixture(now)
		object.ExpiresAt = now.Add(-time.Second)
		db := &fakeStore{t: t,
			getFn: func(context.Context, string) (store.PayloadObject, error) { return object, nil },
			cancelFn: func(context.Context, store.PayloadReadScope, time.Time) (store.PayloadObject, error) {
				return object, nil
			},
		}
		service := newTestService(t, db, testConfig(t.TempDir()), now)
		_, err := service.Finish(context.Background(), Ref{ObjectID: object.ObjectID})
		requireError(t, err, ErrExpired)
	})
	t.Run("not staged", func(t *testing.T) {
		object := objectFixture(now)
		db := &fakeStore{t: t, getFn: func(context.Context, string) (store.PayloadObject, error) { return object, nil }}
		service := newTestService(t, db, testConfig(t.TempDir()), now)
		_, err := service.Finish(context.Background(), Ref{ObjectID: object.ObjectID})
		requireError(t, err, ErrNotStaged)
	})
	t.Run("committed but unpublished", func(t *testing.T) {
		db := &fakeStore{t: t}
		service := newTestService(t, db, testConfig(t.TempDir()), now)
		body := []byte("body")
		object := stagedObject(t, service, db, body, now)
		if _, err := service.Finish(context.Background(), Ref{ObjectID: object.ObjectID}); err != nil {
			t.Fatal(err)
		}
		// Simulate a metadata commit failure: the file is final but the row
		// stays reserved. A repeated finish must verify and publish it.
		object.State = store.PayloadReserved
		db.getFn = func(context.Context, string) (store.PayloadObject, error) { return object, nil }
		ready, err := service.Finish(context.Background(), Ref{ObjectID: object.ObjectID})
		if err != nil || ready.State != store.PayloadReady {
			t.Fatalf("recover finish = %+v err=%v", ready, err)
		}
	})
	t.Run("expired during publish", func(t *testing.T) {
		db := &fakeStore{t: t}
		service := newTestService(t, db, testConfig(t.TempDir()), now)
		object := stagedObject(t, service, db, []byte("body"), now)
		db.confirmFn = func(context.Context, string, int64, []byte, time.Time) (store.PayloadObject, error) {
			return store.PayloadObject{}, store.ErrNotFound
		}
		_, err := service.Finish(context.Background(), Ref{ObjectID: object.ObjectID})
		requireError(t, err, ErrExpired)
	})
	t.Run("corrupt committed file", func(t *testing.T) {
		object := objectFixture(now)
		db := &fakeStore{t: t, getFn: func(context.Context, string) (store.PayloadObject, error) { return object, nil }}
		service := newTestService(t, db, testConfig(t.TempDir()), now)
		mustWriteFile(t, service.finalPath(object.ObjectID), []byte("wrong"))
		_, err := service.Finish(context.Background(), Ref{ObjectID: object.ObjectID})
		requireError(t, err, ErrDigestMismatch)
	})
}

func TestOpenLeasesReaderAndReleasesExactlyOnce(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	body := []byte("readable")
	object := objectFixture(now)
	object.State = store.PayloadReady
	object.Length, object.Reserved, object.Digest = int64(len(body)), int64(len(body)), digestOf(body)
	releases := 0
	db := &fakeStore{t: t,
		getFn: func(context.Context, string) (store.PayloadObject, error) { return object, nil },
		openFn: func(_ context.Context, scope store.PayloadReadScope, lease string, _ time.Time) (store.PayloadObject, error) {
			if !objectIDPattern.MatchString(lease) {
				t.Fatalf("lease id = %q", lease)
			}
			return object, nil
		},
		releaseFn: func(_ context.Context, objectID, lease string) error {
			releases++
			return nil
		},
	}
	service := newTestService(t, db, testConfig(t.TempDir()), now)
	mustWriteFile(t, service.finalPath(object.ObjectID), body)
	reader, got, err := service.Open(context.Background(), Ref{ObjectID: object.ObjectID, HostID: object.HostID})
	if err != nil {
		t.Fatalf("Open: %v", err)
	}
	data, err := io.ReadAll(reader)
	if err != nil || !bytes.Equal(data, body) || got.ObjectID != object.ObjectID {
		t.Fatalf("read = %q object=%+v err=%v", data, got, err)
	}
	if err := reader.Close(); err != nil {
		t.Fatal(err)
	}
	_ = reader.Close()
	if releases != 1 {
		t.Fatalf("releases = %d, want exactly one", releases)
	}
}

func TestOpenClassifiesScopeExpiryAndMissingFile(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	ready := objectFixture(now)
	ready.State = store.PayloadReady
	t.Run("foreign scope", func(t *testing.T) {
		db := &fakeStore{t: t, getFn: func(context.Context, string) (store.PayloadObject, error) { return ready, nil }}
		service := newTestService(t, db, testConfig(t.TempDir()), now)
		_, _, err := service.Open(context.Background(), Ref{ObjectID: ready.ObjectID, HostID: "other"})
		requireError(t, err, ErrForbidden)
	})
	t.Run("not found", func(t *testing.T) {
		db := &fakeStore{t: t, getFn: func(context.Context, string) (store.PayloadObject, error) {
			return store.PayloadObject{}, store.ErrNotFound
		}}
		service := newTestService(t, db, testConfig(t.TempDir()), now)
		_, _, err := service.Open(context.Background(), Ref{ObjectID: ready.ObjectID})
		requireError(t, err, ErrNotFound)
	})
	t.Run("too many readers", func(t *testing.T) {
		object := ready
		object.ReaderCount = MaxReadersPerObject
		db := &fakeStore{t: t, getFn: func(context.Context, string) (store.PayloadObject, error) { return object, nil }}
		service := newTestService(t, db, testConfig(t.TempDir()), now)
		_, _, err := service.Open(context.Background(), Ref{ObjectID: object.ObjectID})
		requireError(t, err, ErrCapacity)
	})
	t.Run("store scope mismatch", func(t *testing.T) {
		db := &fakeStore{t: t,
			getFn: func(context.Context, string) (store.PayloadObject, error) { return ready, nil },
			openFn: func(context.Context, store.PayloadReadScope, string, time.Time) (store.PayloadObject, error) {
				return store.PayloadObject{}, store.ErrPayloadScopeMismatch
			},
		}
		service := newTestService(t, db, testConfig(t.TempDir()), now)
		_, _, err := service.Open(context.Background(), Ref{ObjectID: ready.ObjectID})
		requireError(t, err, ErrForbidden)
	})
	for _, tc := range []struct {
		name   string
		object store.PayloadObject
		want   error
	}{
		{"expired", func() store.PayloadObject { o := ready; o.State = store.PayloadExpired; return o }(), ErrExpired},
		{"reserved", func() store.PayloadObject { o := ready; o.State = store.PayloadReserved; return o }(), ErrNotReady},
		{"past expiry", func() store.PayloadObject { o := ready; o.ExpiresAt = now.Add(-time.Second); return o }(), ErrExpired},
	} {
		t.Run("unavailable "+tc.name, func(t *testing.T) {
			getCalls := 0
			db := &fakeStore{t: t,
				getFn: func(context.Context, string) (store.PayloadObject, error) {
					getCalls++
					if getCalls == 1 {
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
	t.Run("missing file releases the lease without cancelling the row", func(t *testing.T) {
		released, cancelled := false, false
		db := &fakeStore{t: t,
			getFn: func(context.Context, string) (store.PayloadObject, error) { return ready, nil },
			openFn: func(context.Context, store.PayloadReadScope, string, time.Time) (store.PayloadObject, error) {
				return ready, nil
			},
			releaseFn: func(context.Context, string, string) error {
				released = true
				return nil
			},
			cancelFn: func(context.Context, store.PayloadReadScope, time.Time) (store.PayloadObject, error) {
				cancelled = true
				return ready, nil
			},
		}
		service := newTestService(t, db, testConfig(t.TempDir()), now)
		_, _, err := service.Open(context.Background(), Ref{ObjectID: ready.ObjectID})
		requireError(t, err, ErrNotFound)
		if !released || cancelled {
			t.Fatalf("released=%v cancelled=%v, want lease release without a destructive cancel", released, cancelled)
		}
	})
}

func TestCancelRemovesStagedBodyAndMapsErrors(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	t.Run("removes staged", func(t *testing.T) {
		db := &fakeStore{t: t}
		service := newTestService(t, db, testConfig(t.TempDir()), now)
		object := stagedObject(t, service, db, []byte("body"), now)
		db.cancelFn = func(context.Context, store.PayloadReadScope, time.Time) (store.PayloadObject, error) {
			return object, nil
		}
		if err := service.Cancel(context.Background(), Ref{ObjectID: object.ObjectID}); err != nil {
			t.Fatal(err)
		}
		if entries, _ := os.ReadDir(service.tmp); len(entries) != 0 {
			t.Fatalf("staged body survived cancel: %v", entries)
		}
	})
	for _, tc := range []struct {
		name   string
		get    bool
		getErr error
		err    error
		want   error
	}{
		{"scope", true, nil, store.ErrPayloadScopeMismatch, ErrForbidden},
		{"missing", false, store.ErrNotFound, store.ErrNotFound, ErrNotFound},
	} {
		t.Run(tc.name, func(t *testing.T) {
			db := &fakeStore{t: t, cancelFn: func(context.Context, store.PayloadReadScope, time.Time) (store.PayloadObject, error) {
				return store.PayloadObject{}, tc.err
			}}
			if tc.get {
				db.getFn = func(context.Context, string) (store.PayloadObject, error) { return objectFixture(now), nil }
			} else {
				db.getFn = func(context.Context, string) (store.PayloadObject, error) { return store.PayloadObject{}, tc.getErr }
			}
			service := newTestService(t, db, testConfig(t.TempDir()), now)
			requireError(t, service.Cancel(context.Background(), Ref{ObjectID: objectFixture(now).ObjectID}), tc.want)
		})
	}
}

func TestCleanupDeletesFileAndReleasesCapacityOnce(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	object := objectFixture(now)
	object.State = store.PayloadExpired
	object.Reserved = 4 << 20
	deleted := 0
	db := &fakeStore{t: t,
		expireFn: func(context.Context, time.Time, int) ([]store.PayloadObject, error) {
			return []store.PayloadObject{object}, nil
		},
		claimFn: func(context.Context, time.Time, int, time.Duration) ([]store.PayloadObject, error) {
			return []store.PayloadObject{object}, nil
		},
		deletedFn: func(_ context.Context, id string, _ time.Time) (bool, error) {
			deleted++
			return true, nil
		},
		tombstoneFn: func(context.Context, time.Time, int) (int64, error) { return 3, nil },
	}
	service := newTestService(t, db, testConfig(t.TempDir()), now)
	mustWriteFile(t, service.finalPath(object.ObjectID), []byte("body"))
	result, err := service.Cleanup(context.Background())
	if err != nil {
		t.Fatalf("Cleanup: %v", err)
	}
	if result.Expired != 1 || result.Deleted != 1 || result.ReclaimedBytes != 4<<20 || result.Tombstones != 3 {
		t.Fatalf("result = %+v", result)
	}
	if deleted != 1 {
		t.Fatalf("deleted calls = %d", deleted)
	}
	if _, err := os.Stat(service.finalPath(object.ObjectID)); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("file still exists: %v", err)
	}
}

func TestCleanupKeepsChargeWhenFileDeletionFails(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	object := objectFixture(now)
	object.State = store.PayloadExpired
	failures := 0
	db := &fakeStore{t: t,
		expireFn: func(context.Context, time.Time, int) ([]store.PayloadObject, error) { return nil, nil },
		claimFn: func(context.Context, time.Time, int, time.Duration) ([]store.PayloadObject, error) {
			return []store.PayloadObject{object}, nil
		},
		failureFn: func(_ context.Context, id string, _ time.Time, retry time.Duration) error {
			failures++
			if retry <= 0 {
				t.Fatalf("retry = %v", retry)
			}
			return nil
		},
		tombstoneFn: func(context.Context, time.Time, int) (int64, error) { return 0, nil },
	}
	service := newTestService(t, db, testConfig(t.TempDir()), now)
	// A non-empty directory at the final path cannot be removed as a file.
	final := service.finalPath(object.ObjectID)
	if err := os.MkdirAll(final, 0o750); err != nil {
		t.Fatal(err)
	}
	mustWriteFile(t, filepath.Join(final, "child"), []byte("x"))
	result, err := service.Cleanup(context.Background())
	if err == nil {
		t.Fatal("cleanup hid a filesystem failure")
	}
	if result.Failed != 1 || failures != 1 || result.Deleted != 0 {
		t.Fatalf("result = %+v failures=%d", result, failures)
	}
}

func TestCleanupPropagatesStoreFailures(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	for _, tc := range []struct {
		name string
		db   *fakeStore
	}{
		{"expire", &fakeStore{t: t, expireFn: func(context.Context, time.Time, int) ([]store.PayloadObject, error) {
			return nil, errors.New("expire")
		}}},
		{"claim", &fakeStore{t: t,
			expireFn: func(context.Context, time.Time, int) ([]store.PayloadObject, error) { return nil, nil },
			claimFn: func(context.Context, time.Time, int, time.Duration) ([]store.PayloadObject, error) {
				return nil, errors.New("claim")
			},
		}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			service := newTestService(t, tc.db, testConfig(t.TempDir()), now)
			if _, err := service.Cleanup(context.Background()); err == nil {
				t.Fatal("cleanup hid a store failure")
			}
		})
	}
}

func TestCleanupRetryBackoffIsBounded(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	config := testConfig(t.TempDir())
	config.CleanupInterval = time.Second
	config.Retention = 8 * time.Second
	config.UploadTimeout = time.Second
	service := newTestService(t, &fakeStore{t: t}, config, now)
	if got := service.cleanupRetry(0); got != time.Second {
		t.Fatalf("retry 0 = %v", got)
	}
	if got := service.cleanupRetry(3); got != 8*time.Second {
		t.Fatalf("retry 3 = %v", got)
	}
	if got := service.cleanupRetry(20); got != 8*time.Second {
		t.Fatalf("retry 20 = %v", got)
	}
}

func TestRecoverRepairsAndRemovesOrphans(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	service := newTestService(t, &fakeStore{t: t}, testConfig(t.TempDir()), now)
	stalePublished := objectFixture(now)
	stalePublished.ObjectID = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
	staleAbandoned := objectFixture(now)
	staleAbandoned.ObjectID = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
	missingReady := objectFixture(now)
	missingReady.ObjectID = "cccccccccccccccccccccccccccccccc"
	missingReady.State = store.PayloadReady
	presentReady := objectFixture(now)
	presentReady.ObjectID = "dddddddddddddddddddddddddddddddd"
	presentReady.State = store.PayloadReady
	orphanID := "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"
	deletedID := "ffffffffffffffffffffffffffffffff"
	confirmed, cancelled := 0, 0
	db := &fakeStore{t: t,
		expireFn: func(context.Context, time.Time, int) ([]store.PayloadObject, error) {
			return []store.PayloadObject{staleAbandoned}, nil
		},
		staleFn: func(context.Context, time.Time, int) ([]store.PayloadObject, error) {
			return []store.PayloadObject{stalePublished, staleAbandoned}, nil
		},
		readyPageFn: func(context.Context, string, int) ([]store.PayloadObject, error) {
			return []store.PayloadObject{missingReady, presentReady}, nil
		},
		getFn: func(_ context.Context, id string) (store.PayloadObject, error) {
			switch id {
			case stalePublished.ObjectID:
				return stalePublished, nil
			case presentReady.ObjectID:
				return presentReady, nil
			case orphanID:
				return store.PayloadObject{}, store.ErrNotFound
			case deletedID:
				object := objectFixture(now)
				object.ObjectID = id
				object.State = store.PayloadDeleted
				return object, nil
			default:
				return store.PayloadObject{}, errors.New("unexpected get")
			}
		},
		confirmFn: func(context.Context, string, int64, []byte, time.Time) (store.PayloadObject, error) {
			confirmed++
			return stalePublished, nil
		},
		cancelFn: func(_ context.Context, scope store.PayloadReadScope, _ time.Time) (store.PayloadObject, error) {
			cancelled++
			return store.PayloadObject{}, nil
		},
	}
	service.store = db
	mustWriteFile(t, service.finalPath(stalePublished.ObjectID), []byte("body"))
	mustWriteFile(t, service.finalPath(presentReady.ObjectID), []byte("body"))
	mustWriteFile(t, service.finalPath(orphanID), []byte("orphan"))
	mustWriteFile(t, service.finalPath(deletedID), []byte("deleted"))
	if err := service.Recover(context.Background()); err != nil {
		t.Fatalf("Recover: %v", err)
	}
	if confirmed != 1 {
		t.Fatalf("confirmed = %d", confirmed)
	}
	// Expired batch, abandoned stale reservation and three missing ready files
	// (including the expired batch object) are cancelled. The exact count is
	// asserted as at least the two stale/missing decisions.
	if cancelled < 2 {
		t.Fatalf("cancelled = %d", cancelled)
	}
	if _, err := os.Stat(service.finalPath(orphanID)); !errors.Is(err, os.ErrNotExist) {
		t.Fatal("orphan file was not removed")
	}
	if _, err := os.Stat(service.finalPath(deletedID)); !errors.Is(err, os.ErrNotExist) {
		t.Fatal("deleted metadata file was not removed")
	}
}

func TestRecoverAggregatesStoreFailures(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	db := &fakeStore{t: t,
		expireFn:    func(context.Context, time.Time, int) ([]store.PayloadObject, error) { return nil, errors.New("expire") },
		staleFn:     func(context.Context, time.Time, int) ([]store.PayloadObject, error) { return nil, errors.New("stale") },
		readyPageFn: func(context.Context, string, int) ([]store.PayloadObject, error) { return nil, errors.New("page") },
	}
	service := newTestService(t, db, testConfig(t.TempDir()), now)
	if err := service.Recover(context.Background()); err == nil {
		t.Fatal("recover hid store failures")
	}
}

func TestScanManagedFilesIgnoresUnknownNames(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	service := newTestService(t, &fakeStore{t: t}, testConfig(t.TempDir()), now)
	mustWriteFile(t, filepath.Join(service.tmp, "not-an-object.tmp"), []byte("x"))
	mustWriteFile(t, filepath.Join(service.tmp, "0123456789abcdef0123456789abcdef.zzz.tmp"), []byte("x"))
	mustWriteFile(t, filepath.Join(service.objects, "zz", "0123456789abcdef0123456789abcdef.bin"), []byte("x"))
	files, err := service.scanManagedFiles(10)
	if err != nil {
		t.Fatalf("scan: %v", err)
	}
	if len(files) != 1 || files[0].ObjectID != "0123456789abcdef0123456789abcdef" {
		t.Fatalf("files = %+v", files)
	}
}

func TestOpenReaderStopsAtExpiryAndCancellation(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	body := []byte("readable")
	object := objectFixture(now)
	object.State = store.PayloadReady
	object.Length, object.Reserved, object.Digest = int64(len(body)), int64(len(body)), digestOf(body)
	cancelled := false
	db := &fakeStore{t: t,
		getFn: func(context.Context, string) (store.PayloadObject, error) {
			return object, nil
		},
		openFn: func(context.Context, store.PayloadReadScope, string, time.Time) (store.PayloadObject, error) {
			return object, nil
		},
		releaseFn: func(context.Context, string, string) error { return nil },
		cancelFn: func(context.Context, store.PayloadReadScope, time.Time) (store.PayloadObject, error) {
			cancelled = true
			return object, nil
		},
	}
	clock := now
	service := newTestService(t, db, testConfig(t.TempDir()), now)
	service.now = func() time.Time { return clock }
	mustWriteFile(t, service.finalPath(object.ObjectID), body)
	reader, _, err := service.Open(context.Background(), Ref{ObjectID: object.ObjectID, HostID: object.HostID})
	if err != nil {
		t.Fatalf("Open: %v", err)
	}
	defer func() { _ = reader.Close() }()

	clock = now.Add(2 * time.Hour)
	if _, err := reader.Read(make([]byte, 1)); !errors.Is(err, os.ErrDeadlineExceeded) {
		t.Fatalf("expired read err = %v", err)
	}

	clock = now
	reader2, _, err := service.Open(context.Background(), Ref{ObjectID: object.ObjectID, HostID: object.HostID})
	if err != nil {
		t.Fatalf("second Open: %v", err)
	}
	defer func() { _ = reader2.Close() }()
	if err := service.Cancel(context.Background(), Ref{ObjectID: object.ObjectID, HostID: object.HostID}); err != nil {
		t.Fatalf("Cancel: %v", err)
	}
	if !cancelled {
		t.Fatal("cancel did not reach the store")
	}
	if _, err := reader2.Read(make([]byte, 1)); !errors.Is(err, os.ErrDeadlineExceeded) {
		t.Fatalf("cancelled read err = %v", err)
	}
}
