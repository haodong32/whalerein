//go:build integration && unix

package payload_test

import (
	"bytes"
	"context"
	"crypto/sha256"
	"errors"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/payload"
	"github.com/whalerein/whalerein/access/internal/store"
	"github.com/whalerein/whalerein/access/internal/store/storetest"
)

func integrationConfig(root string) payload.Config {
	return payload.Config{
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
		CleanupInterval:       time.Second,
		CleanupBatch:          10,
		UploadTimeout:         20 * time.Millisecond,
		ReadTimeout:           time.Minute,
	}
}

func integrationInput(body []byte) payload.ReserveInput {
	digest := sha256.Sum256(body)
	return payload.ReserveInput{
		HostID:    "host-1",
		DeviceID:  "device-1",
		RequestID: "request-1",
		Purpose:   "result",
		Length:    int64(len(body)),
		Digest:    digest[:],
	}
}

// A new service instance over the same database and directory repairs a stale
// reservation without publishing incomplete data.
func TestStorageRestartAbandonsStaleReservation(t *testing.T) {
	db, _ := storetest.New(t)
	ctx := context.Background()
	root := t.TempDir()
	first, err := payload.New(payload.Dependencies{Store: db, Config: integrationConfig(root)})
	if err != nil {
		t.Fatal(err)
	}
	object, err := first.Reserve(ctx, integrationInput([]byte("sealed")))
	if err != nil {
		t.Fatal(err)
	}
	if err := first.Write(ctx, payload.Ref{ObjectID: object.ObjectID}, bytes.NewReader([]byte("sealed"))); err != nil {
		t.Fatal(err)
	}
	time.Sleep(50 * time.Millisecond)

	second, err := payload.New(payload.Dependencies{Store: db, Config: integrationConfig(root)})
	if err != nil {
		t.Fatal(err)
	}
	if err := second.Recover(ctx); err != nil {
		t.Fatalf("Recover: %v", err)
	}
	if _, _, err := second.Open(ctx, payload.Ref{ObjectID: object.ObjectID}); !errors.Is(err, payload.ErrExpired) && !errors.Is(err, payload.ErrNotFound) && !errors.Is(err, payload.ErrNotReady) {
		t.Fatalf("incomplete object opened: %v", err)
	}
	result, err := second.Cleanup(ctx)
	if err != nil {
		t.Fatalf("Cleanup: %v", err)
	}
	if result.Deleted == 0 {
		t.Fatalf("stale reservation was not reclaimed: %+v", result)
	}
	stats, err := second.Stats(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if stats.ChargedBytes != 0 {
		t.Fatalf("charged bytes after restart cleanup = %d", stats.ChargedBytes)
	}
}

// A file renamed before the metadata commit is published by reconciliation.
func TestStorageRestartPublishesCommittedFile(t *testing.T) {
	db, _ := storetest.New(t)
	ctx := context.Background()
	root := t.TempDir()
	first, err := payload.New(payload.Dependencies{Store: db, Config: integrationConfig(root)})
	if err != nil {
		t.Fatal(err)
	}
	body := []byte("sealed")
	object, err := first.Reserve(ctx, integrationInput(body))
	if err != nil {
		t.Fatal(err)
	}
	if err := first.Write(ctx, payload.Ref{ObjectID: object.ObjectID}, bytes.NewReader(body)); err != nil {
		t.Fatal(err)
	}
	// Simulate a crash between the atomic rename and the metadata commit.
	final := filepath.Join(root, "objects", object.ObjectID[:2], object.ObjectID+".bin")
	if err := os.MkdirAll(filepath.Dir(final), 0o750); err != nil {
		t.Fatal(err)
	}
	staged, err := os.ReadDir(filepath.Join(root, "tmp"))
	if err != nil || len(staged) != 1 {
		t.Fatalf("staged files = %v err=%v", staged, err)
	}
	if err := os.Rename(filepath.Join(root, "tmp", staged[0].Name()), final); err != nil {
		t.Fatal(err)
	}
	time.Sleep(50 * time.Millisecond)

	second, err := payload.New(payload.Dependencies{Store: db, Config: integrationConfig(root)})
	if err != nil {
		t.Fatal(err)
	}
	if err := second.Recover(ctx); err != nil {
		t.Fatalf("Recover: %v", err)
	}
	reader, ready, err := second.Open(ctx, payload.Ref{ObjectID: object.ObjectID, HostID: "host-1"})
	if err != nil {
		t.Fatalf("Open after recovery: %v", err)
	}
	data := new(bytes.Buffer)
	if _, err := data.ReadFrom(reader); err != nil {
		t.Fatal(err)
	}
	_ = reader.Close()
	if !bytes.Equal(data.Bytes(), body) || ready.State != store.PayloadReady {
		t.Fatalf("recovered object = %q state=%q", data.Bytes(), ready.State)
	}
}

// A failed file deletion keeps the charge and is retried by a later pass.
func TestStorageCleanupRetriesFailedDeletion(t *testing.T) {
	db, _ := storetest.New(t)
	ctx := context.Background()
	root := t.TempDir()
	config := integrationConfig(root)
	config.CleanupInterval = time.Millisecond
	service, err := payload.New(payload.Dependencies{Store: db, Config: config})
	if err != nil {
		t.Fatal(err)
	}
	body := []byte("sealed")
	object, err := service.Reserve(ctx, integrationInput(body))
	if err != nil {
		t.Fatal(err)
	}
	if err := service.Write(ctx, payload.Ref{ObjectID: object.ObjectID}, bytes.NewReader(body)); err != nil {
		t.Fatal(err)
	}
	// Cancel closes admission and makes the object cleanup-eligible.
	if err := service.Cancel(ctx, payload.Ref{ObjectID: object.ObjectID}); err != nil {
		t.Fatal(err)
	}
	final := filepath.Join(root, "objects", object.ObjectID[:2], object.ObjectID+".bin")
	if err := os.MkdirAll(filepath.Join(final, "child"), 0o750); err != nil {
		t.Fatal(err)
	}
	result, err := service.Cleanup(ctx)
	if err == nil || result.Failed == 0 {
		t.Fatalf("cleanup hid a deletion failure: %+v err=%v", result, err)
	}
	stats, err := service.Stats(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if stats.ChargedBytes == 0 {
		t.Fatal("failed deletion released the charge")
	}
	if err := os.RemoveAll(final); err != nil {
		t.Fatal(err)
	}
	time.Sleep(50 * time.Millisecond)
	result, err = service.Cleanup(ctx)
	if err != nil || result.Deleted == 0 {
		t.Fatalf("retry = %+v err=%v", result, err)
	}
	stats, err = service.Stats(ctx)
	if err != nil || stats.ChargedBytes != 0 {
		t.Fatalf("charge after retry = %+v err=%v", stats, err)
	}
}

// The configured reserve is compared with actual filesystem free space.
func TestStorageAdmissionRefusesBelowFreeSpaceReserve(t *testing.T) {
	db, _ := storetest.New(t)
	ctx := context.Background()
	config := integrationConfig(t.TempDir())
	config.FreeSpaceReserveBytes = 1 << 62
	service, err := payload.New(payload.Dependencies{Store: db, Config: config})
	if err != nil {
		t.Fatal(err)
	}
	if err := service.Ready(ctx); !errors.Is(err, payload.ErrHeadroom) {
		t.Fatalf("readiness = %v, want headroom", err)
	}
	if _, err := service.Reserve(ctx, integrationInput([]byte("sealed"))); !errors.Is(err, payload.ErrHeadroom) {
		t.Fatalf("reserve = %v, want headroom", err)
	}
}
