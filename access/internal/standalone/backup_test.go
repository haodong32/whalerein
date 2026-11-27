package standalone

import (
	"context"
	"crypto/rand"
	"errors"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
)

func fixture(t *testing.T) (context.Context, *store.Store, string, string, []byte) {
	t.Helper()
	ctx := context.Background()
	dir := t.TempDir()
	dbPath := filepath.Join(dir, "access.db")
	data, err := store.Open(ctx, dbPath)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(data.Close)
	if err := data.Migrate(ctx); err != nil {
		t.Fatal(err)
	}
	key := make([]byte, 32)
	if _, err := rand.Read(key); err != nil {
		t.Fatal(err)
	}
	return ctx, data, dir, "installation-1", key
}

// AC-28(a): a trusted, quiesced, current backup preserves pairings and the
// revocation floor.
func TestCurrentTrustedBackupVerifies(t *testing.T) {
	ctx, data, dir, installation, key := fixture(t)
	backupPath := filepath.Join(dir, "backup.db")
	_, err := Create(ctx, data, backupPath, installation, key, 7, time.Now())
	if err != nil {
		t.Fatal(err)
	}
	verified, err := Verify(backupPath, manifestPath(backupPath), key, installation, 7)
	if err != nil {
		t.Fatal(err)
	}
	if verified.AuthorityRevision != 7 || !verified.Quiesced {
		t.Fatalf("unexpected manifest: %#v", verified)
	}
}

// AC-28(b): an older backup cannot reinstate a revoked peer.
func TestStaleBackupIsRefused(t *testing.T) {
	ctx, data, dir, installation, key := fixture(t)
	backupPath := filepath.Join(dir, "backup.db")
	if _, err := Create(ctx, data, backupPath, installation, key, 4, time.Now()); err != nil {
		t.Fatal(err)
	}
	if _, err := Verify(backupPath, manifestPath(backupPath), key, installation, 5); !errors.Is(err, ErrStaleBackup) {
		t.Fatalf("stale backup error = %v", err)
	}
}

// AC-28(c): a missing backup or manifest requires fresh authority.
func TestMissingOrForgedBackupIsRefused(t *testing.T) {
	_, _, dir, installation, key := fixture(t)
	backupPath := filepath.Join(dir, "missing.db")
	if _, err := Verify(backupPath, manifestPath(backupPath), key, installation, 0); !errors.Is(err, ErrUntrustedBackup) {
		t.Fatalf("missing backup error = %v", err)
	}

	ctx, data, dir, installation, key := fixture(t)
	backupPath = filepath.Join(dir, "backup.db")
	if _, err := Create(ctx, data, backupPath, installation, key, 1, time.Now()); err != nil {
		t.Fatal(err)
	}
	wrong := make([]byte, 32)
	if _, err := rand.Read(wrong); err != nil {
		t.Fatal(err)
	}
	if _, err := Verify(backupPath, manifestPath(backupPath), wrong, installation, 0); !errors.Is(err, ErrUntrustedBackup) {
		t.Fatalf("wrong installation key error = %v", err)
	}
	// Changing the database after the manifest must fail the digest check.
	if err := os.WriteFile(backupPath, []byte("not the snapshot"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := Verify(backupPath, manifestPath(backupPath), key, installation, 0); !errors.Is(err, ErrUntrustedBackup) {
		t.Fatalf("modified backup error = %v", err)
	}
}

func TestUnquiescedManifestIsRefused(t *testing.T) {
	ctx, data, dir, installation, key := fixture(t)
	backupPath := filepath.Join(dir, "backup.db")
	manifest, err := Create(ctx, data, backupPath, installation, key, 1, time.Now())
	if err != nil {
		t.Fatal(err)
	}
	manifest.Quiesced = false
	if err := writeManifest(manifestPath(backupPath), manifest); err != nil {
		t.Fatal(err)
	}
	if _, err := Verify(backupPath, manifestPath(backupPath), key, installation, 0); !errors.Is(err, ErrUnquiesced) {
		t.Fatalf("unquiesced backup error = %v", err)
	}
}

func manifestPath(databasePath string) string { return databasePath + ".json" }

func TestCreateValidationAndWriteFailures(t *testing.T) {
	ctx, data, dir, installation, key := fixture(t)
	if _, err := Create(ctx, data, filepath.Join(dir, "b.db"), "", key, 1, time.Now()); !errors.Is(err, ErrUntrustedBackup) {
		t.Fatalf("missing installation = %v", err)
	}
	if _, err := Create(ctx, data, filepath.Join(dir, "b.db"), installation, []byte("short"), 1, time.Now()); !errors.Is(err, ErrUntrustedBackup) {
		t.Fatalf("short key = %v", err)
	}
	// A manifest path that is a directory makes the atomic write fail after
	// the snapshot, which must be reported rather than silently accepted.
	backup := filepath.Join(dir, "backup.db")
	if err := os.Mkdir(backup+".json", 0o700); err != nil {
		t.Fatal(err)
	}
	if _, err := Create(ctx, data, backup, installation, key, 1, time.Now()); err == nil {
		t.Fatal("manifest write failure was accepted")
	}
}

func TestVerifyManifestShapeFailures(t *testing.T) {
	ctx, data, dir, installation, key := fixture(t)
	backup := filepath.Join(dir, "backup.db")
	if _, err := Create(ctx, data, backup, installation, key, 1, time.Now()); err != nil {
		t.Fatal(err)
	}
	manifest := manifestPath(backup)
	// Invalid JSON.
	if err := os.WriteFile(manifest, []byte("not json"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := Verify(backup, manifest, key, installation, 0); !errors.Is(err, ErrUntrustedBackup) {
		t.Fatalf("invalid JSON = %v", err)
	}
	// A fresh backup with the wrong installation identity.
	second := filepath.Join(dir, "backup2.db")
	if _, err := Create(ctx, data, second, installation, key, 1, time.Now()); err != nil {
		t.Fatal(err)
	}
	if _, err := Verify(second, manifestPath(second), key, "other-installation", 0); !errors.Is(err, ErrUntrustedBackup) {
		t.Fatalf("wrong installation = %v", err)
	}
	// fileDigest on a missing database.
	if err := os.Remove(second); err != nil {
		t.Fatal(err)
	}
	if _, err := Verify(second, manifestPath(second), key, installation, 0); !errors.Is(err, ErrUntrustedBackup) {
		t.Fatalf("missing database = %v", err)
	}
	// A manifest that is a directory cannot be written.
	third := filepath.Join(dir, "backup3.db")
	if err := os.Mkdir(manifestPath(third), 0o700); err != nil {
		t.Fatal(err)
	}
	if _, err := Create(ctx, data, third, installation, key, 1, time.Now()); err == nil {
		t.Fatal("manifest directory write was accepted")
	}
}

func TestRestoreInstallsATrustedBackupAtAnEmptyTarget(t *testing.T) {
	ctx, data, dir, installation, key := fixture(t)
	backupPath := filepath.Join(dir, "backup.db")
	if _, err := Create(ctx, data, backupPath, installation, key, 7, time.Now()); err != nil {
		t.Fatal(err)
	}
	target := filepath.Join(dir, "restored", "access.db")
	manifest, err := Restore(ctx, backupPath, manifestPath(backupPath), target, key, installation, 7)
	if err != nil {
		t.Fatal(err)
	}
	if manifest.AuthorityRevision != 7 {
		t.Fatalf("manifest = %#v", manifest)
	}
	restored, err := store.Open(ctx, target)
	if err != nil {
		t.Fatal(err)
	}
	defer restored.Close()
	if err := restored.Migrate(ctx); err != nil {
		t.Fatalf("restored database cannot open: %v", err)
	}
	if _, err := os.Stat(target); err != nil {
		t.Fatal(err)
	}
}

func TestRestoreRefusesStaleOrExistingTargets(t *testing.T) {
	ctx, data, dir, installation, key := fixture(t)
	backupPath := filepath.Join(dir, "backup.db")
	if _, err := Create(ctx, data, backupPath, installation, key, 4, time.Now()); err != nil {
		t.Fatal(err)
	}
	target := filepath.Join(dir, "restored.db")
	if _, err := Restore(ctx, backupPath, manifestPath(backupPath), target, key, installation, 5); !errors.Is(err, ErrStaleBackup) {
		t.Fatalf("stale restore = %v", err)
	}
	if _, err := os.Stat(target); !errors.Is(err, os.ErrNotExist) {
		t.Fatal("stale restore created a target")
	}
	if err := os.WriteFile(target, []byte("existing"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := Restore(ctx, backupPath, manifestPath(backupPath), target, key, installation, 4); err == nil {
		t.Fatal("restore overwrote an existing target")
	}
	if _, err := Restore(ctx, backupPath, manifestPath(backupPath), "", key, installation, 4); !errors.Is(err, ErrUntrustedBackup) {
		t.Fatalf("empty target = %v", err)
	}
}

func TestRestoreReportsTargetPreparationFailures(t *testing.T) {
	ctx, data, dir, installation, key := fixture(t)
	backupPath := filepath.Join(dir, "backup.db")
	if _, err := Create(ctx, data, backupPath, installation, key, 1, time.Now()); err != nil {
		t.Fatal(err)
	}
	file := filepath.Join(dir, "not-a-directory")
	if err := os.WriteFile(file, []byte("x"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := Restore(ctx, backupPath, manifestPath(backupPath), filepath.Join(file, "access.db"), key, installation, 1); err == nil {
		t.Fatal("restore under a file succeeded")
	}
	canceled, stop := context.WithCancel(ctx)
	stop()
	if _, err := Restore(canceled, backupPath, manifestPath(backupPath), filepath.Join(dir, "canceled.db"), key, installation, 1); !errors.Is(err, context.Canceled) {
		t.Fatalf("canceled restore = %v", err)
	}
	symlinkTarget := filepath.Join(dir, "symlink-target")
	if err := os.Symlink(filepath.Join(dir, "does-not-exist"), symlinkTarget); err != nil {
		t.Fatal(err)
	}
	if _, err := Restore(ctx, backupPath, manifestPath(backupPath), symlinkTarget, key, installation, 1); err == nil {
		t.Fatal("restore replaced a dangling symlink")
	}
	readonly := filepath.Join(dir, "readonly")
	if err := os.Mkdir(readonly, 0o500); err != nil {
		t.Fatal(err)
	}
	defer func() { _ = os.Chmod(readonly, 0o700) }()
	if _, err := Restore(ctx, backupPath, manifestPath(backupPath), filepath.Join(readonly, "access.db"), key, installation, 1); err == nil {
		t.Fatal("restore wrote into a read-only directory")
	}
}

func TestCreateReportsSnapshotAndTemporaryManifestFailures(t *testing.T) {
	ctx, data, dir, installation, key := fixture(t)
	if _, err := Create(ctx, data, filepath.Join(dir, "b.db"), installation, key, 1, time.Now()); err != nil {
		t.Fatal(err)
	}
	// A temporary manifest path that is a directory fails before rename.
	backup := filepath.Join(dir, "tempfail.db")
	if err := os.Mkdir(backup+".json.tmp", 0o700); err != nil {
		t.Fatal(err)
	}
	if _, err := Create(ctx, data, backup, installation, key, 1, time.Now()); err == nil {
		t.Fatal("temporary manifest failure was accepted")
	}
	// A closed store cannot snapshot.
	closed := filepath.Join(dir, "closed.db")
	data.Close()
	if _, err := Create(ctx, data, closed, installation, key, 1, time.Now()); err == nil {
		t.Fatal("snapshot of a closed store succeeded")
	}
}
