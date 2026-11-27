// Package standalone owns the fresh-authority backup rules for a Standalone
// Access installation.
//
// Central is not involved. A replacement disk may preserve local pairings only
// when the operator supplies a consistent backup plus a manifest that proves it
// covers the final committed authorization state. An unknown or older backup
// must start a fresh authority and require explicit pairing.
package standalone

import (
	"context"
	"crypto/hmac"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
)

// ManifestVersion is the only accepted manifest shape.
const ManifestVersion = "whalerein/standalone-backup-v1"

var (
	ErrUntrustedBackup = errors.New("standalone backup is not trusted")
	ErrStaleBackup     = errors.New("standalone backup predates the final authorization state")
	ErrUnquiesced      = errors.New("standalone backup was not taken while writes were quiesced")
)

// Manifest is stored beside the database copy. It is never secret, but its
// HMAC binds it to the installation credential so an attacker cannot relabel an
// old, permissive snapshot as current.
type Manifest struct {
	Version           string `json:"version"`
	InstallationID    string `json:"installation_id"`
	CreatedAtUnixMs   int64  `json:"created_at_unix_ms"`
	AuthorityRevision uint64 `json:"authority_revision"`
	Quiesced          bool   `json:"quiesced"`
	DatabaseSHA256    string `json:"database_sha256"`
	Mac               string `json:"mac"`
}

func macInput(manifest Manifest) []byte {
	canonical := fmt.Sprintf("%s\n%s\n%d\n%d\n%t\n%s",
		manifest.Version, manifest.InstallationID, manifest.CreatedAtUnixMs,
		manifest.AuthorityRevision, manifest.Quiesced, manifest.DatabaseSHA256)
	return []byte(canonical)
}

// Create writes a consistent copy and a signed manifest. The caller must have
// stopped Access and the installation key must be the one from the original
// installation, never a key generated after the disk was lost.
func Create(ctx context.Context, data *store.Store, destination, installationID string, installationKey []byte, authorityRevision uint64, now time.Time) (Manifest, error) {
	if len(installationID) == 0 || len(installationKey) < 32 {
		return Manifest{}, ErrUntrustedBackup
	}
	if err := os.MkdirAll(filepath.Dir(destination), 0o700); err != nil {
		return Manifest{}, fmt.Errorf("prepare backup directory: %w", err)
	}
	if err := data.Snapshot(ctx, destination); err != nil {
		return Manifest{}, err
	}
	digest, err := fileDigest(destination)
	if err != nil {
		return Manifest{}, err
	}
	manifest := Manifest{
		Version: ManifestVersion, InstallationID: installationID,
		CreatedAtUnixMs: now.UTC().UnixMilli(), AuthorityRevision: authorityRevision,
		Quiesced: true, DatabaseSHA256: digest,
	}
	mac := hmac.New(sha256.New, installationKey)
	mac.Write(macInput(manifest))
	manifest.Mac = hex.EncodeToString(mac.Sum(nil))
	if err := writeManifest(destination+".json", manifest); err != nil {
		return Manifest{}, err
	}
	return manifest, nil
}

// Verify checks trust, freshness and the exact database digest. `floor` is the
// greatest authorization revision the operator can independently prove was
// committed before the disk was lost. A backup below that floor could
// reinstate a revoked peer and is refused.
func Verify(databasePath, manifestPath string, installationKey []byte, installationID string, floor uint64) (Manifest, error) {
	raw, err := os.ReadFile(manifestPath)
	if err != nil {
		return Manifest{}, ErrUntrustedBackup
	}
	var manifest Manifest
	if err := json.Unmarshal(raw, &manifest); err != nil {
		return Manifest{}, ErrUntrustedBackup
	}
	if manifest.Version != ManifestVersion || manifest.InstallationID != installationID || len(installationKey) < 32 {
		return Manifest{}, ErrUntrustedBackup
	}
	if !manifest.Quiesced {
		return Manifest{}, ErrUnquiesced
	}
	mac := hmac.New(sha256.New, installationKey)
	mac.Write(macInput(manifest))
	expected, err := hex.DecodeString(manifest.Mac)
	if err != nil || !hmac.Equal(expected, mac.Sum(nil)) {
		return Manifest{}, ErrUntrustedBackup
	}
	if manifest.AuthorityRevision < floor {
		return Manifest{}, ErrStaleBackup
	}
	digest, err := fileDigest(databasePath)
	if err != nil || digest != manifest.DatabaseSHA256 {
		return Manifest{}, ErrUntrustedBackup
	}
	return manifest, nil
}

func writeManifest(path string, manifest Manifest) error {
	raw, err := json.MarshalIndent(manifest, "", "  ")
	if err != nil {
		return err
	}
	temp := path + ".tmp"
	if err := os.WriteFile(temp, append(raw, '\n'), 0o600); err != nil {
		return err
	}
	return os.Rename(temp, path)
}

func fileDigest(path string) (string, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return "", err
	}
	sum := sha256.Sum256(raw)
	return hex.EncodeToString(sum[:]), nil
}

// Restore verifies a trusted backup and atomically installs it at an empty
// target path. It never overwrites an existing database: an operator must
// explicitly remove or move the old disk first, which prevents a stale restore
// from silently merging two authorities.
func Restore(ctx context.Context, backupPath, manifestPath, targetPath string, installationKey []byte, installationID string, floor uint64) (Manifest, error) {
	if targetPath == "" {
		return Manifest{}, ErrUntrustedBackup
	}
	if err := ctx.Err(); err != nil {
		return Manifest{}, err
	}
	manifest, err := Verify(backupPath, manifestPath, installationKey, installationID, floor)
	if err != nil {
		return Manifest{}, err
	}
	if _, err := os.Stat(targetPath); err == nil {
		return Manifest{}, fmt.Errorf("restore target already exists: %s", targetPath)
	} else if !errors.Is(err, os.ErrNotExist) {
		return Manifest{}, fmt.Errorf("inspect restore target: %w", err)
	}
	raw, err := os.ReadFile(backupPath)
	if err != nil {
		return Manifest{}, fmt.Errorf("read backup: %w", err)
	}
	if err := os.MkdirAll(filepath.Dir(targetPath), 0o700); err != nil {
		return Manifest{}, fmt.Errorf("prepare restore directory: %w", err)
	}
	temp := targetPath + ".restore.tmp"
	if err := os.WriteFile(temp, raw, 0o600); err != nil {
		return Manifest{}, fmt.Errorf("write restore target: %w", err)
	}
	// Link, unlike rename, never replaces an existing target. The target is a
	// fully written and renamed temp file, so a concurrent restore cannot
	// overwrite another installation.
	if err := os.Link(temp, targetPath); err != nil {
		_ = os.Remove(temp)
		return Manifest{}, fmt.Errorf("install restore target: %w", err)
	}
	if err := os.Remove(temp); err != nil {
		return Manifest{}, fmt.Errorf("remove restore temporary file: %w", err)
	}
	return manifest, nil
}
