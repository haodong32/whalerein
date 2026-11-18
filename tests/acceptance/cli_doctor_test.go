//go:build acceptance

package acceptance

import (
	"bytes"
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"testing"
)

type cliRun struct {
	exit   int
	stdout string
	stderr string
}

func repoRoot(t *testing.T) string {
	t.Helper()
	working, err := os.Getwd()
	if err != nil {
		t.Fatalf("working directory: %v", err)
	}
	return filepath.Clean(filepath.Join(working, "..", ".."))
}

// cliBinary builds or locates the product CLI. It never substitutes another
// executable: a missing toolchain fails this acceptance check.
func cliBinary(t *testing.T) string {
	t.Helper()
	root := repoRoot(t)
	candidate := os.Getenv("WHALEREIN_ACCEPTANCE_CLI")
	if candidate != "" {
		if !filepath.IsAbs(candidate) {
			candidate = filepath.Join(root, candidate)
		}
		if _, err := os.Stat(candidate); err != nil {
			t.Fatalf("WHALEREIN_ACCEPTANCE_CLI is not an executable file: %v", err)
		}
		return candidate
	}
	name := "whalerein"
	if runtime.GOOS == "windows" {
		name += ".exe"
	}
	candidate = filepath.Join(root, "target", "debug", name)
	if _, err := os.Stat(candidate); err == nil {
		return candidate
	}
	build := exec.Command("cargo", "build", "--package", "whalerein-cli", "--bin", "whalerein")
	build.Dir = root
	if output, err := build.CombinedOutput(); err != nil {
		t.Fatalf("build whalerein CLI: %v (%s)", err, output)
	}
	if _, err := os.Stat(candidate); err != nil {
		t.Fatalf("built CLI is missing: %v", err)
	}
	return candidate
}

func runCLI(t *testing.T, dataDir string, extraEnv []string, args ...string) cliRun {
	t.Helper()
	configPath := filepath.Join(t.TempDir(), "client.toml")
	config := "version = 1\nservice = \"standalone\"\ndata_dir = " + strconv.Quote(dataDir) + "\n"
	if err := os.WriteFile(configPath, []byte(config), 0o600); err != nil {
		t.Fatalf("write selected client TOML: %v", err)
	}
	command := exec.Command(cliBinary(t), append([]string{"--config", configPath}, args...)...)
	command.Env = append(os.Environ(), extraEnv...)
	var stdout, stderr bytes.Buffer
	command.Stdout = &stdout
	command.Stderr = &stderr
	err := command.Run()
	exit := 0
	if err != nil {
		if exitError, ok := err.(*exec.ExitError); ok {
			exit = exitError.ExitCode()
		} else {
			t.Fatalf("run CLI: %v", err)
		}
	}
	return cliRun{exit: exit, stdout: stdout.String(), stderr: stderr.String()}
}

func decodeEnvelope(t *testing.T, run cliRun) map[string]any {
	t.Helper()
	var envelope map[string]any
	if err := json.Unmarshal([]byte(run.stdout), &envelope); err != nil {
		t.Fatalf("stdout is not one JSON envelope: %v (%q)", err, run.stdout)
	}
	if envelope["schema"] != "whalerein/client/v1" {
		t.Fatalf("envelope schema = %v", envelope["schema"])
	}
	return envelope
}

// TestCLIDoctorDegradedEnvironment exercises the diagnostic entry point without
// editing CLI code. It checks the observable contract that matters when a user
// is already in trouble: doctor never panics, export failures are structured and
// leave no partial file, successful exports are private, and a symlinked export
// either refuses to write or leaves a private, redacted bundle.
//

func TestCLIDoctorDegradedEnvironment(t *testing.T) {
	t.Run("legacy state is rejected without mutation", func(t *testing.T) {
		dataDir := t.TempDir()
		legacy := filepath.Join(dataDir, "config.json")
		fixture := []byte(`{"schema":"whalerein/v1/config"}`)
		if err := os.WriteFile(legacy, fixture, 0o600); err != nil {
			t.Fatal(err)
		}
		run := runCLI(t, dataDir, nil, "doctor", "--json")
		envelope := decodeEnvelope(t, run)
		errorValue, _ := envelope["error"].(map[string]any)
		if run.exit != 2 || errorValue["code"] != "invalid_configuration" {
			t.Fatalf("legacy opening returned exit %d, code %v", run.exit, errorValue["code"])
		}
		contents, err := os.ReadFile(legacy)
		if err != nil || !bytes.Equal(contents, fixture) {
			t.Fatal("legacy state changed")
		}
	})

	directory := t.TempDir()
	t.Run("export rejects a directory target as a usage error", func(t *testing.T) {
		run := runCLI(t, t.TempDir(), nil, "doctor", "--export", directory, "--yes", "--json")
		if run.exit != 2 {
			t.Fatalf("directory export exit = %d, want 2", run.exit)
		}
		envelope := decodeEnvelope(t, run)
		errorValue, _ := envelope["error"].(map[string]any)
		if errorValue["code"] != "invalid_argument" {
			t.Fatalf("error code = %v, want invalid_argument", errorValue["code"])
		}
	})

	t.Run("export into an unwritable directory fails without a partial file", func(t *testing.T) {
		if runtime.GOOS == "windows" {
			t.Skip("POSIX directory permissions are the tested mechanism; Windows ACL behavior is not equivalent")
		}
		root := t.TempDir()
		readOnly := filepath.Join(root, "readonly")
		if err := os.Mkdir(readOnly, 0o500); err != nil {
			t.Fatalf("create read-only directory: %v", err)
		}
		target := filepath.Join(readOnly, "bundle.json")
		run := runCLI(t, t.TempDir(), nil, "doctor", "--export", target, "--yes", "--json")
		if run.exit == 0 {
			t.Fatal("export to an unwritable directory succeeded")
		}
		decodeEnvelope(t, run)
		if _, err := os.Stat(target); !os.IsNotExist(err) {
			t.Fatalf("failed export left a partial file: %v", err)
		}
	})

	t.Run("successful export is private and redacted", func(t *testing.T) {
		dataDir := t.TempDir()
		target := filepath.Join(t.TempDir(), "bundle.json")
		run := runCLI(t, dataDir, nil, "doctor", "--export", target, "--yes", "--json")
		if run.exit != 0 {
			t.Fatalf("export exit = %d: %s", run.exit, run.stdout)
		}
		envelope := decodeEnvelope(t, run)
		data, ok := envelope["data"].(map[string]any)
		if !ok || data["bytes"] == nil {
			t.Fatalf("export envelope has no manifest data: %v", envelope)
		}
		info, err := os.Stat(target)
		if err != nil {
			t.Fatalf("stat export: %v", err)
		}
		if runtime.GOOS != "windows" && info.Mode().Perm() != 0o600 {
			t.Fatalf("export mode = %o, want 600", info.Mode().Perm())
		}
		bundle, err := os.ReadFile(target)
		if err != nil {
			t.Fatalf("read export: %v", err)
		}
		if !bytes.Contains(bundle, []byte(`"redactions"`)) || !bytes.Contains(bundle, []byte(`"schema"`)) {
			t.Fatal("export bundle is missing the redaction manifest")
		}
	})

	t.Run("export through a symlink is private and redacted or refused", func(t *testing.T) {
		if runtime.GOOS == "windows" {
			t.Skip("symlink creation requires privileges that vary on Windows; the Linux CLI job covers this case")
		}
		root := t.TempDir()
		target := filepath.Join(root, "target.json")
		if err := os.WriteFile(target, []byte("sentinel"), 0o644); err != nil {
			t.Fatalf("write symlink target: %v", err)
		}
		link := filepath.Join(root, "bundle.json")
		if err := os.Symlink(target, link); err != nil {
			t.Fatalf("create symlink: %v", err)
		}
		run := runCLI(t, t.TempDir(), nil, "doctor", "--export", link, "--yes", "--json")
		contents, err := os.ReadFile(target)
		if err != nil {
			t.Fatalf("read symlink target: %v", err)
		}
		if run.exit != 0 {
			if string(contents) != "sentinel" {
				t.Fatalf("refused symlink export modified the target: %q", contents)
			}
			return
		}
		info, err := os.Stat(target)
		if err != nil {
			t.Fatalf("stat symlink target: %v", err)
		}
		if info.Mode().Perm() != 0o600 {
			t.Fatalf("symlink export mode = %o, want 600", info.Mode().Perm())
		}
		if strings.Contains(string(contents), "sentinel") {
			t.Fatal("symlink export did not replace the sentinel with a bundle")
		}
		var bundle map[string]any
		if err := json.Unmarshal(contents, &bundle); err != nil {
			t.Fatalf("symlink export is not JSON: %v", err)
		}
		if bundle["schema"] != "whalerein/v1" {
			t.Fatalf("symlink export schema = %v", bundle["schema"])
		}
	})
}
