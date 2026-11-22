//go:build unix

package payload

import (
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

type failingReader struct{ err error }

func (r failingReader) Read([]byte) (int, error) { return 0, r.err }

func TestWriteRejectsInvalidInputAndReaderFailures(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	body := []byte("body")
	newDB := func() *fakeStore {
		return &fakeStore{t: t, getFn: func(context.Context, string) (store.PayloadObject, error) {
			object := objectFixture(now)
			object.Length, object.Reserved, object.Digest = int64(len(body)), int64(len(body)), digestOf(body)
			return object, nil
		}}
	}
	t.Run("invalid ref", func(t *testing.T) {
		service := newTestService(t, newDB(), testConfig(t.TempDir()), now)
		requireError(t, service.Write(context.Background(), Ref{ObjectID: "bad"}, strings.NewReader("body")), ErrInvalid)
	})
	t.Run("nil body", func(t *testing.T) {
		service := newTestService(t, newDB(), testConfig(t.TempDir()), now)
		requireError(t, service.Write(context.Background(), Ref{ObjectID: objectFixture(now).ObjectID}, nil), ErrInvalid)
	})
	t.Run("get failure", func(t *testing.T) {
		db := &fakeStore{t: t, getFn: func(context.Context, string) (store.PayloadObject, error) {
			return store.PayloadObject{}, errors.New("db down")
		}}
		service := newTestService(t, db, testConfig(t.TempDir()), now)
		if err := service.Write(context.Background(), Ref{ObjectID: objectFixture(now).ObjectID}, strings.NewReader("body")); err == nil {
			t.Fatal("write hid a store failure")
		}
	})
	t.Run("reader failure", func(t *testing.T) {
		service := newTestService(t, newDB(), testConfig(t.TempDir()), now)
		if err := service.Write(context.Background(), Ref{ObjectID: objectFixture(now).ObjectID}, failingReader{err: errors.New("read")}); err == nil {
			t.Fatal("write hid a reader failure")
		}
		if entries, _ := os.ReadDir(service.tmp); len(entries) != 0 {
			t.Fatalf("temp survived: %v", entries)
		}
	})
	t.Run("cancelled context", func(t *testing.T) {
		service := newTestService(t, newDB(), testConfig(t.TempDir()), now)
		ctx, cancel := context.WithCancel(context.Background())
		cancel()
		if err := service.Write(ctx, Ref{ObjectID: objectFixture(now).ObjectID}, strings.NewReader("body")); err == nil {
			t.Fatal("write hid a cancelled context")
		}
	})
	t.Run("staging directory unavailable", func(t *testing.T) {
		service := newTestService(t, newDB(), testConfig(t.TempDir()), now)
		if err := os.RemoveAll(service.tmp); err != nil {
			t.Fatal(err)
		}
		mustWriteFile(t, service.tmp, []byte("not a directory"))
		if err := service.Write(context.Background(), Ref{ObjectID: objectFixture(now).ObjectID}, strings.NewReader("body")); err == nil {
			t.Fatal("write hid an unusable staging directory")
		}
	})
}

func TestFinishRejectsStagedMismatchAndPublishFailures(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	t.Run("invalid ref and get failure", func(t *testing.T) {
		service := newTestService(t, &fakeStore{t: t}, testConfig(t.TempDir()), now)
		if _, err := service.Finish(context.Background(), Ref{ObjectID: "bad"}); !errors.Is(err, ErrInvalid) {
			t.Fatalf("invalid ref = %v", err)
		}
		service.store = &fakeStore{t: t, getFn: func(context.Context, string) (store.PayloadObject, error) {
			return store.PayloadObject{}, errors.New("db down")
		}}
		if _, err := service.Finish(context.Background(), Ref{ObjectID: objectFixture(now).ObjectID}); err == nil {
			t.Fatal("finish hid a store failure")
		}
	})
	t.Run("foreign scope", func(t *testing.T) {
		db := &fakeStore{t: t, getFn: func(context.Context, string) (store.PayloadObject, error) {
			return objectFixture(now), nil
		}}
		service := newTestService(t, db, testConfig(t.TempDir()), now)
		_, err := service.Finish(context.Background(), Ref{ObjectID: objectFixture(now).ObjectID, HostID: "other"})
		requireError(t, err, ErrForbidden)
	})
	t.Run("staged digest mismatch", func(t *testing.T) {
		db := &fakeStore{t: t}
		service := newTestService(t, db, testConfig(t.TempDir()), now)
		object := stagedObject(t, service, db, []byte("body"), now)
		path, _ := service.stagedPath(object.ObjectID)
		mustWriteFile(t, path, []byte("evil"))
		_, err := service.Finish(context.Background(), Ref{ObjectID: object.ObjectID})
		requireError(t, err, ErrDigestMismatch)
	})
	t.Run("staged length mismatch", func(t *testing.T) {
		db := &fakeStore{t: t}
		service := newTestService(t, db, testConfig(t.TempDir()), now)
		object := stagedObject(t, service, db, []byte("body"), now)
		path, _ := service.stagedPath(object.ObjectID)
		mustWriteFile(t, path, []byte("longer"))
		_, err := service.Finish(context.Background(), Ref{ObjectID: object.ObjectID})
		requireError(t, err, ErrLengthMismatch)
	})
	t.Run("staged file disappears", func(t *testing.T) {
		db := &fakeStore{t: t}
		service := newTestService(t, db, testConfig(t.TempDir()), now)
		object := stagedObject(t, service, db, []byte("body"), now)
		path, _ := service.stagedPath(object.ObjectID)
		if err := os.Remove(path); err != nil {
			t.Fatal(err)
		}
		_, err := service.Finish(context.Background(), Ref{ObjectID: object.ObjectID})
		if err == nil {
			t.Fatal("finish hid a missing staged file")
		}
	})
	t.Run("publish directory unavailable", func(t *testing.T) {
		db := &fakeStore{t: t}
		service := newTestService(t, db, testConfig(t.TempDir()), now)
		object := stagedObject(t, service, db, []byte("body"), now)
		if err := os.RemoveAll(service.objects); err != nil {
			t.Fatal(err)
		}
		mustWriteFile(t, service.objects, []byte("not a directory"))
		_, err := service.Finish(context.Background(), Ref{ObjectID: object.ObjectID})
		if err == nil {
			t.Fatal("finish hid an unusable publish directory")
		}
	})
	t.Run("metadata commit failure", func(t *testing.T) {
		db := &fakeStore{t: t}
		service := newTestService(t, db, testConfig(t.TempDir()), now)
		object := stagedObject(t, service, db, []byte("body"), now)
		db.confirmFn = func(context.Context, string, int64, []byte, time.Time) (store.PayloadObject, error) {
			return store.PayloadObject{}, errors.New("commit")
		}
		_, err := service.Finish(context.Background(), Ref{ObjectID: object.ObjectID})
		if err == nil {
			t.Fatal("finish hid a metadata commit failure")
		}
		if _, statErr := os.Stat(service.finalPath(object.ObjectID)); statErr != nil {
			t.Fatal("committed file was removed after a metadata failure")
		}
	})
}

func TestOpenRejectsWrongSizeAndStoreFailure(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	ready := objectFixture(now)
	ready.State = store.PayloadReady
	t.Run("invalid ref", func(t *testing.T) {
		service := newTestService(t, &fakeStore{t: t}, testConfig(t.TempDir()), now)
		_, _, err := service.Open(context.Background(), Ref{ObjectID: "bad"})
		requireError(t, err, ErrInvalid)
	})
	t.Run("get failure", func(t *testing.T) {
		db := &fakeStore{t: t, getFn: func(context.Context, string) (store.PayloadObject, error) {
			return store.PayloadObject{}, errors.New("db down")
		}}
		service := newTestService(t, db, testConfig(t.TempDir()), now)
		if _, _, err := service.Open(context.Background(), Ref{ObjectID: ready.ObjectID}); err == nil {
			t.Fatal("open hid a store failure")
		}
	})
	t.Run("lease store failure", func(t *testing.T) {
		db := &fakeStore{t: t,
			getFn: func(context.Context, string) (store.PayloadObject, error) { return ready, nil },
			openFn: func(context.Context, store.PayloadReadScope, string, time.Time) (store.PayloadObject, error) {
				return store.PayloadObject{}, errors.New("lease")
			},
		}
		service := newTestService(t, db, testConfig(t.TempDir()), now)
		if _, _, err := service.Open(context.Background(), Ref{ObjectID: ready.ObjectID}); err == nil {
			t.Fatal("open hid a lease failure")
		}
	})
	t.Run("wrong file size", func(t *testing.T) {
		released, cancelled := false, false
		db := &fakeStore{t: t,
			getFn: func(context.Context, string) (store.PayloadObject, error) { return ready, nil },
			openFn: func(context.Context, store.PayloadReadScope, string, time.Time) (store.PayloadObject, error) {
				return ready, nil
			},
			releaseFn: func(context.Context, string, string) error {
				released = true
				return errors.New("release")
			},
			cancelFn: func(context.Context, store.PayloadReadScope, time.Time) (store.PayloadObject, error) {
				cancelled = true
				return ready, nil
			},
		}
		service := newTestService(t, db, testConfig(t.TempDir()), now)
		mustWriteFile(t, service.finalPath(ready.ObjectID), []byte("short"))
		_, _, err := service.Open(context.Background(), Ref{ObjectID: ready.ObjectID})
		requireError(t, err, ErrNotFound)
		if !released || cancelled {
			t.Fatalf("released=%v cancelled=%v, want a non-destructive miss", released, cancelled)
		}
	})
	t.Run("classify store failure", func(t *testing.T) {
		calls := 0
		db := &fakeStore{t: t,
			getFn: func(context.Context, string) (store.PayloadObject, error) {
				calls++
				if calls == 1 {
					return ready, nil
				}
				return store.PayloadObject{}, errors.New("db down")
			},
			openFn: func(context.Context, store.PayloadReadScope, string, time.Time) (store.PayloadObject, error) {
				return store.PayloadObject{}, store.ErrNotFound
			},
		}
		service := newTestService(t, db, testConfig(t.TempDir()), now)
		if _, _, err := service.Open(context.Background(), Ref{ObjectID: ready.ObjectID}); err == nil {
			t.Fatal("open hid a classification failure")
		}
	})
}

func TestReadyRejectsCancelledContextAndUnwritableDirectory(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	config := testConfig(t.TempDir())
	service := newTestService(t, &fakeStore{t: t}, config, now)
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if err := service.Ready(ctx); err == nil {
		t.Fatal("cancelled readiness succeeded")
	}
	if err := os.RemoveAll(service.tmp); err != nil {
		t.Fatal(err)
	}
	if err := service.Ready(context.Background()); err == nil {
		t.Fatal("missing quota directory was ready")
	}
}

func TestCleanupRemovesStagedTempAndToleratesMissingDirectory(t *testing.T) {
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
	mustWriteFile(t, filepath.Join(service.tmp, object.ObjectID+"."+strings.Repeat("a", 32)+".tmp"), []byte("x"))
	if _, err := service.Cleanup(context.Background()); err != nil {
		t.Fatalf("Cleanup: %v", err)
	}
	entries, _ := os.ReadDir(service.tmp)
	if len(entries) != 0 {
		t.Fatalf("staged temp survived cleanup: %v", entries)
	}
	// A missing staging directory is not a cleanup failure.
	service.discardStaged(object.ObjectID)
	if err := os.RemoveAll(service.tmp); err != nil {
		t.Fatal(err)
	}
	if _, err := service.Cleanup(context.Background()); err != nil {
		t.Fatalf("Cleanup without tmp: %v", err)
	}
}

func TestScanManagedFilesReportsDirectoryErrorsAndHonorsLimit(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	service := newTestService(t, &fakeStore{t: t}, testConfig(t.TempDir()), now)
	mustWriteFile(t, filepath.Join(service.tmp, "0123456789abcdef0123456789abcdef."+strings.Repeat("a", 32)+".tmp"), []byte("x"))
	mustWriteFile(t, filepath.Join(service.tmp, "fedcba9876543210fedcba9876543210."+strings.Repeat("b", 32)+".tmp"), []byte("x"))
	files, err := service.scanManagedFiles(1)
	if err != nil || len(files) != 1 {
		t.Fatalf("limit scan = %+v err=%v", files, err)
	}
	if err := os.RemoveAll(service.objects); err != nil {
		t.Fatal(err)
	}
	mustWriteFile(t, service.objects, []byte("not a directory"))
	if _, err := service.scanManagedFiles(10); err == nil {
		t.Fatal("unreadable object root was ignored")
	}
}

func TestOwnerIDFallsBackToNodeIDAndOverrideWins(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	var owners []string
	db := &fakeStore{
		t:       t,
		statsFn: func(context.Context, time.Time) (store.PayloadStats, error) { return store.PayloadStats{}, nil },
		reserveFn: func(_ context.Context, reservation store.PayloadReservation, _ store.PayloadLimits) (store.PayloadAdmission, error) {
			owners = append(owners, reservation.OwnerID)
			return store.PayloadAdmission{Object: store.PayloadObject{ObjectID: reservation.ObjectID}}, nil
		},
	}
	config := testConfig(t.TempDir())
	service := newTestService(t, db, config, now)
	input := ReserveInput{HostID: "host", RequestID: "request", Purpose: "result", Length: 4, Digest: digestOf([]byte("body"))}
	if _, err := service.Reserve(context.Background(), input); err != nil {
		t.Fatal(err)
	}
	config.OwnerID = "owner-1"
	service = newTestService(t, db, config, now)
	service.config.OwnerID = "owner-1"
	if _, err := service.Reserve(context.Background(), input); err != nil {
		t.Fatal(err)
	}
	if len(owners) != 2 || owners[0] != "node-1" || owners[1] != "owner-1" {
		t.Fatalf("owners = %v", owners)
	}
}

func TestCloseObjectAndReleaseLeaseLogFailures(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	db := &fakeStore{t: t,
		getFn: func(context.Context, string) (store.PayloadObject, error) {
			object := objectFixture(now)
			object.ExpiresAt = now.Add(-time.Second)
			return object, nil
		},
		cancelFn: func(context.Context, store.PayloadReadScope, time.Time) (store.PayloadObject, error) {
			return store.PayloadObject{}, errors.New("cancel")
		},
		releaseFn: func(context.Context, string, string) error { return errors.New("release") },
	}
	service := newTestService(t, db, testConfig(t.TempDir()), now)
	requireError(t, service.Write(context.Background(), Ref{ObjectID: objectFixture(now).ObjectID}, strings.NewReader("body")), ErrExpired)
	service.releaseLease("0123456789abcdef0123456789abcdef", "fedcba9876543210fedcba9876543210")
}

func TestContextReaderStopsOnCancelledContext(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	reader := contextReader{ctx: ctx, reader: strings.NewReader("body")}
	if _, err := reader.Read(make([]byte, 4)); !errors.Is(err, context.Canceled) {
		t.Fatalf("read = %v", err)
	}
	if _, err := io.ReadAll(io.LimitReader(strings.NewReader("body"), 4)); err != nil {
		t.Fatal(err)
	}
}

func TestWriteHonorsUploadTimeoutAndOpenHonorsReadTimeout(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	t.Run("upload timeout", func(t *testing.T) {
		db := &fakeStore{t: t, getFn: func(context.Context, string) (store.PayloadObject, error) {
			object := objectFixture(now)
			object.Length, object.Reserved, object.Digest = 4, 4, digestOf([]byte("body"))
			return object, nil
		}}
		config := testConfig(t.TempDir())
		config.UploadTimeout = time.Nanosecond
		service := newTestService(t, db, config, now)
		if err := service.Write(context.Background(), Ref{ObjectID: objectFixture(now).ObjectID}, strings.NewReader("body")); err == nil {
			t.Fatal("write ignored the configured upload timeout")
		}
	})
	t.Run("read timeout", func(t *testing.T) {
		body := []byte("body")
		object := objectFixture(now)
		object.State = store.PayloadReady
		object.Length, object.Reserved, object.Digest = int64(len(body)), int64(len(body)), digestOf(body)
		db := &fakeStore{t: t,
			getFn: func(context.Context, string) (store.PayloadObject, error) { return object, nil },
			openFn: func(context.Context, store.PayloadReadScope, string, time.Time) (store.PayloadObject, error) {
				return object, nil
			},
			releaseFn: func(context.Context, string, string) error {
				return nil
			},
		}
		config := testConfig(t.TempDir())
		config.ReadTimeout = time.Nanosecond
		service := newTestService(t, db, config, now)
		mustWriteFile(t, service.finalPath(object.ObjectID), body)
		reader, _, err := service.Open(context.Background(), Ref{ObjectID: object.ObjectID})
		if err != nil {
			t.Fatal(err)
		}
		defer func() { _ = reader.Close() }()
		time.Sleep(2 * time.Millisecond)
		if _, err := reader.Read(make([]byte, 4)); !errors.Is(err, os.ErrDeadlineExceeded) {
			t.Fatalf("read = %v, want deadline exceeded", err)
		}
	})
}
