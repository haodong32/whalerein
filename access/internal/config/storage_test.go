package config

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func storageYAML(root string) string {
	return minimalYAML + `
storage:
  enabled: true
  root_dir: "` + root + `"
  node_id: "node-1"
  owner_id: "owner-1"
  max_object_bytes: 1048576
  max_host_bytes: 4194304
  max_node_bytes: 8388608
  max_objects: 10
  max_active_uploads: 4
  free_space_reserve_bytes: 1024
  retention: 2h
  cleanup_interval: 30s
  cleanup_batch: 25
  upload_timeout: 5m
  read_timeout: 45s
`
}

func TestStorageDefaultsAreDisabledButBounded(t *testing.T) {
	cfg, err := Parse([]byte(minimalYAML), env(validEnv()))
	if err != nil {
		t.Fatalf("Parse: %v", err)
	}
	if cfg.Storage.Enabled {
		t.Fatal("temporary storage must be opt-in")
	}
	if cfg.Storage.MaxObjectBytes <= 0 || cfg.Storage.MaxHostBytes < cfg.Storage.MaxObjectBytes ||
		cfg.Storage.MaxNodeBytes < cfg.Storage.MaxHostBytes || cfg.Storage.Retention.D() <= 0 ||
		cfg.Storage.CleanupInterval.D() <= 0 || cfg.Storage.CleanupBatch <= 0 ||
		cfg.Storage.UploadTimeout.D() <= 0 || cfg.Storage.ReadTimeout.D() <= 0 {
		t.Fatalf("storage defaults are not bounded: %+v", cfg.Storage)
	}
	if cfg.Storage.NodeID == "" || cfg.Storage.RootDir == "" || cfg.Storage.FreeSpaceReserveBytes <= 0 {
		t.Fatalf("storage identity defaults missing: %+v", cfg.Storage)
	}
}

func TestStorageConfigurationIsAcceptedAndPrepared(t *testing.T) {
	root := filepath.Join(t.TempDir(), "quota")
	cfg, err := Parse([]byte(storageYAML(root)), env(validEnv()))
	if err != nil {
		t.Fatalf("Parse: %v", err)
	}
	if !cfg.Storage.Enabled || cfg.Storage.OwnerID != "owner-1" || cfg.Storage.Retention.D() != 2*time.Hour {
		t.Fatalf("storage = %+v", cfg.Storage)
	}
	for _, dir := range []string{root, filepath.Join(root, "objects"), filepath.Join(root, "tmp")} {
		info, err := os.Stat(dir)
		if err != nil || !info.IsDir() {
			t.Fatalf("prepared directory %s: %v", dir, err)
		}
	}
}

func TestStorageValidationRejectsUnsafeSettings(t *testing.T) {
	root := t.TempDir()
	cases := []struct {
		name   string
		mutate func(*Config)
		want   string
	}{
		{"relative root", func(c *Config) { c.Storage.RootDir = "relative" }, "root_dir"},
		{"missing node", func(c *Config) { c.Storage.NodeID = "" }, "node_id"},
		{"zero object", func(c *Config) { c.Storage.MaxObjectBytes = 0 }, "byte limits"},
		{"host below object", func(c *Config) { c.Storage.MaxHostBytes = c.Storage.MaxObjectBytes - 1 }, "max_host_bytes"},
		{"node below host", func(c *Config) { c.Storage.MaxNodeBytes = c.Storage.MaxHostBytes - 1 }, "max_node_bytes"},
		{"zero objects", func(c *Config) { c.Storage.MaxObjects = 0 }, "max_objects"},
		{"uploads above objects", func(c *Config) { c.Storage.MaxActiveUploads = c.Storage.MaxObjects + 1 }, "max_active_uploads"},
		{"zero reserve", func(c *Config) { c.Storage.FreeSpaceReserveBytes = 0 }, "free_space_reserve"},
		{"zero retention", func(c *Config) { c.Storage.Retention = 0 }, "retention"},
		{"zero cleanup interval", func(c *Config) { c.Storage.CleanupInterval = 0 }, "cleanup_interval"},
		{"zero batch", func(c *Config) { c.Storage.CleanupBatch = 0 }, "cleanup_batch"},
		{"zero upload timeout", func(c *Config) { c.Storage.UploadTimeout = 0 }, "upload_timeout"},
		{"zero read timeout", func(c *Config) { c.Storage.ReadTimeout = 0 }, "read_timeout"},
		{"upload beyond retention", func(c *Config) { c.Storage.UploadTimeout = c.Storage.Retention + 1 }, "must not exceed"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			cfg := Default()
			cfg.Storage = Default().Storage
			cfg.Storage.Enabled = true
			cfg.Storage.RootDir = root
			tc.mutate(&cfg)
			err := cfg.Validate()
			if err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("error = %v, want %q", err, tc.want)
			}
		})
	}
}

func TestStorageValidationRejectsUnwritableRoot(t *testing.T) {
	root := t.TempDir()
	blocked := filepath.Join(root, "file")
	if err := os.WriteFile(blocked, []byte("x"), 0o600); err != nil {
		t.Fatal(err)
	}
	cfg := Default()
	cfg.Storage.Enabled = true
	cfg.Storage.RootDir = blocked
	err := cfg.Validate()
	if err == nil || !strings.Contains(err.Error(), "root_dir") {
		t.Fatalf("error = %v", err)
	}
}

func TestStorageRejectsUnknownKeys(t *testing.T) {
	yaml := minimalYAML + "\nstorage:\n  enabled: false\n  unexpected: true\n"
	if _, err := Parse([]byte(yaml), env(validEnv())); err == nil {
		t.Fatal("an unknown storage key was accepted")
	}
}

func TestStorageRedactionCarriesOnlyBoundedSettings(t *testing.T) {
	cfg, err := Parse([]byte(storageYAML(t.TempDir())), env(validEnv()))
	if err != nil {
		t.Fatalf("Parse: %v", err)
	}
	redacted := cfg.Redacted()
	for _, key := range []string{
		"storage_enabled", "storage_root_configured", "storage_node_id",
		"storage_max_object_bytes", "storage_max_host_bytes", "storage_max_node_bytes",
		"storage_max_objects", "storage_max_active_uploads", "storage_free_space_reserve",
		"storage_retention",
	} {
		if _, ok := redacted[key]; !ok {
			t.Fatalf("redacted summary missing %s", key)
		}
	}
	if redacted["storage_node_id"] != "node-1" || redacted["storage_enabled"] != true {
		t.Fatalf("redacted storage = %+v", redacted)
	}
	for _, value := range redacted {
		text, ok := value.(string)
		if !ok {
			continue
		}
		if strings.Contains(text, "secret") {
			t.Fatalf("redacted summary leaked a secret-like value: %q", text)
		}
	}
}
