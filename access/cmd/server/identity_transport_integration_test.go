//go:build integration

package main

// Opt-in full stack: actual CLI, SQL grants, Noise pairing and encrypted plugin
// results over the isolated 1 MiB broker. It never reads user credentials.
import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/config"
	storeerrors "github.com/whalerein/whalerein/access/internal/store"
	"github.com/whalerein/whalerein/access/internal/store/storetest"
)

// When the CLI pairs, exact large encrypted Messages arrive; revocation removes access,
// while explicit re-pairing retains key identity without duplicate grants or peers.
func TestIdentityAndEncryptedTransportAgainstIsolatedStack(t *testing.T) {
	if runtime.GOOS != "linux" {
		t.Skip("This isolated Secret Service fixture requires Linux D-Bus.")
	}
	brokerURL := os.Getenv("WHALEREIN_TEST_MQTT_URL")
	if brokerURL == "" {
		brokerURL = "mqtt://127.0.0.1:1884"
	}
	db := storetest.NewSchema(t)
	store := db.Open(t)
	if _, err := store.EnsureHost(context.Background(), "host-flow"); err != nil {
		t.Fatal(err)
	}
	port := freePort(t)
	base := fmt.Sprintf("http://127.0.0.1:%d", port)
	internalPort := freePort(t)
	// The case-scoped Mosquitto plugin posts deferred CONNECT decisions to a
	// fixed callback port configured before Access starts.
	if value := os.Getenv("WHALEREIN_TEST_INTERNAL_PORT"); value != "" {
		parsed, err := strconv.Atoi(value)
		if err != nil {
			t.Fatal("invalid WHALEREIN_TEST_INTERNAL_PORT")
		}
		internalPort = parsed
	}
	path := writeConfig(t, configValues{publicPort: port, internalPort: internalPort, databaseURL: db.DSN, brokerURL: brokerURL})
	cfg, err := config.Load(path)
	if err != nil {
		t.Fatal(err)
	}
	cfg.PublicURL = base
	cfg.Broker.PublicURL = brokerURL
	cfg.Broker.ClientID = "identity-transport-access"
	if password := os.Getenv("WHALEREIN_TEST_BROKER_PASSWORD"); password != "" {
		cfg.Broker.Password = password
	}
	// node_runtime.mjs presents this synthetic Host provisioning credential.
	cfg.HostProvisioningToken = "fixture-host"
	// The plugin reads its callback token from the broker configuration file.
	if token := os.Getenv("WHALEREIN_TEST_INTERNAL_TOKEN"); token != "" {
		cfg.Internal.CallbackToken = token
	}
	cfg.Broker.ConnectTimeout = config.Duration(5 * time.Second)
	cfg.Broker.PublishTimeout = config.Duration(10 * time.Second)
	cfg.Pairing.ClaimTimeout = config.Duration(5 * time.Second)
	cfg.Revocation.AckTimeout = config.Duration(5 * time.Second)
	cfg.HTTP.MaxBodyBytes = 24 << 20
	cfg.HTTP.WriteTimeout = config.Duration(20 * time.Second)
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	accessLog := io.Writer(io.Discard)
	logPath := os.Getenv("WHALEREIN_TEST_ACCESS_LOG")
	if logPath == "" {
		if info, err := os.Stat("/reports"); err == nil && info.IsDir() {
			logPath = "/reports/access.log"
		}
	}
	var logFile *os.File
	if path := logPath; path != "" {
		opened, err := os.Create(path)
		if err != nil {
			t.Fatal("access log unavailable")
		}
		logFile = opened
		accessLog = opened
	}
	defer func() {
		if logFile != nil {
			_ = logFile.Close()
		}
	}()
	stopped := make(chan error, 1)
	go func() { stopped <- runServer(ctx, cfg, slog.New(slog.NewTextHandler(accessLog, nil))) }()
	defer func() {
		cancel()
		if err := <-stopped; err != nil {
			t.Error("isolated Access did not stop cleanly")
		}
	}()
	if err := waitFor(t, 5*time.Second, func() bool {
		response, err := http.Get(base + "/readyz")
		if err != nil {
			return false
		}
		response.Body.Close()
		return response.StatusCode == 200
	}); err != nil {
		t.Fatal("isolated Access not ready")
	}
	root, err := filepath.Abs("../../..")
	if err != nil {
		t.Fatal(err)
	}
	host := exec.CommandContext(ctx, "node", filepath.Join(root, "tests/integration/drivers/transport/node_runtime.mjs"))
	host.Env = append(os.Environ(), "WHALEREIN_TEST_MQTT_URL="+brokerURL, "WHALEREIN_TEST_STATE_DIR="+t.TempDir())
	output, err := host.StdoutPipe()
	if err != nil {
		t.Fatal(err)
	}
	input, err := host.StdinPipe()
	if err != nil {
		t.Fatal(err)
	}
	host.Stderr = io.Discard
	if err := host.Start(); err != nil {
		t.Fatal(err)
	}
	defer func() { _ = host.Process.Kill(); _ = host.Wait() }()
	scan := bufio.NewScanner(output)
	if !scan.Scan() || scan.Text() != "runtime-ready" {
		t.Fatal("isolated plugin not ready")
	}
	if err := waitFor(t, 3*time.Second, func() bool {
		key, err := store.HostAuthorizationKey(ctx, "host-flow")
		return err == nil && len(key) == 32
	}); err != nil {
		t.Fatal("host identity not registered")
	}
	data := t.TempDir()
	selected := filepath.Join(data, "client.toml")
	state := filepath.Join(data, "instance", "state")
	if err := os.WriteFile(selected, []byte("version = 1\nservice = \"standalone\"\ndata_dir = \"instance\"\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	keyringEnv := nativeFixtureKeyring(t, root)
	binary := filepath.Join(root, "target/debug/whalerein")
	invoke := func(input string, args ...string) []byte {
		command := exec.CommandContext(ctx, binary, append([]string{"--json", "--config", selected}, args...)...)
		command.Env = keyringEnv
		command.Stdin = strings.NewReader(input)
		var stdout bytes.Buffer
		command.Stdout = &stdout
		command.Stderr = io.Discard
		if err := command.Run(); err != nil {
			t.Fatalf("isolated CLI %s failed: code=%s", args[0], cliErrorCode(stdout.Bytes()))
		}
		return stdout.Bytes()
	}
	invite := func() string {
		request, err := http.NewRequest(http.MethodPost, base+"/v1/pairing/invitations", strings.NewReader(`{"hostId":"host-flow"}`))
		if err != nil {
			t.Fatal(err)
		}
		request.Header.Set("Authorization", "Bearer "+cfg.AdminToken)
		request.Header.Set("Content-Type", "application/json")
		response, err := http.DefaultClient.Do(request)
		if err != nil {
			t.Fatal("invitation request failed")
		}
		defer response.Body.Close()
		var body struct {
			URI string `json:"pairingUri"`
		}
		if response.StatusCode != 200 || json.NewDecoder(response.Body).Decode(&body) != nil || body.URI == "" {
			t.Fatal("invitation unavailable")
		}
		return body.URI
	}
	pair := func() string {
		var result struct {
			Data struct {
				DeviceID string `json:"device_id"`
			}
		}
		if json.Unmarshal(invoke(invite(), "host", "connect", "--invite-stdin", "--yes"), &result) != nil || result.Data.DeviceID == "" {
			t.Fatal("pairing outcome missing identity")
		}
		return result.Data.DeviceID
	}
	original := pair()
	assertLargeMessage := func(raw []byte) string {
		t.Helper()
		decoder := json.NewDecoder(bytes.NewReader(raw))
		count, id := 0, ""
		for {
			var envelope struct {
				Data struct {
					Event struct {
						Type    string `json:"type"`
						ID      string `json:"message_id"`
						Role    string `json:"role"`
						Session struct {
							Host    string `json:"host_id"`
							Session string `json:"session_id"`
						} `json:"session"`
						Blocks []struct {
							Type string `json:"type"`
							Text string `json:"text"`
						} `json:"blocks"`
					} `json:"event"`
				} `json:"data"`
			}
			if err := decoder.Decode(&envelope); err == io.EOF {
				break
			} else if err != nil {
				t.Fatal("large Message output was not valid JSON")
			}
			event := envelope.Data.Event
			if event.Type != "message" {
				continue
			}
			count++
			// PLUG-18 bounds a single projected message to the history page
			// budget (with a truncation notice), so the visible text must be a
			// non-trivial prefix of the 2 MiB native body, not the whole body.
			if event.ID == "" || event.Role != "ASSISTANT" || event.Session.Host != "host-flow" || event.Session.Session != "session-flow" || len(event.Blocks) == 0 || event.Blocks[0].Type != "text" || len(event.Blocks[0].Text) < 128*1024 || !strings.HasPrefix(strings.Repeat("x", 2*1024*1024), event.Blocks[0].Text) {
				t.Fatal("large encrypted Message changed content, identity, role, or session scope")
			}
			id = event.ID
		}
		if count != 1 {
			t.Fatal("large encrypted Message was missing or delivered more than once")
		}
		return id
	}
	// The scoped state arrives after the watch synchronizes the encrypted baseline.
	live := exec.CommandContext(ctx, binary, "--json", "--config", selected,
		"session", "watch", "session-flow", "--once", "--timeout", "15")
	live.Env = keyringEnv
	live.Stderr = io.Discard
	liveStream, err := live.StdoutPipe()
	if err != nil {
		t.Fatal(err)
	}
	if err := live.Start(); err != nil {
		t.Fatal(err)
	}
	defer func() { _ = live.Process.Kill(); _ = live.Wait() }()
	var liveOutput bytes.Buffer
	stream := bufio.NewReader(liveStream)
	for {
		line, err := stream.ReadBytes('\n')
		if err != nil {
			_ = live.Wait()
			t.Fatal("live CLI subscription unavailable")
		}
		liveOutput.Write(line)
		var outcome struct {
			Data struct {
				Event struct {
					Type string `json:"type"`
				} `json:"event"`
			} `json:"data"`
		}
		if json.Unmarshal(line, &outcome) == nil && outcome.Data.Event.Type == "state" {
			break
		}
	}
	if _, err := io.WriteString(input, "emit\n"); err != nil {
		t.Fatal(err)
	}
	_, readErr := io.Copy(&liveOutput, stream)
	if err := live.Wait(); err != nil || readErr != nil || liveOutput.Len() < 128*1024 {
		code := ""
		for _, line := range bytes.Split(liveOutput.Bytes(), []byte("\n")) {
			var outcome struct {
				Error *struct {
					Code string `json:"code"`
				} `json:"error"`
			}
			if json.Unmarshal(line, &outcome) == nil && outcome.Error != nil {
				code = outcome.Error.Code
			}
		}
		t.Fatalf("large live delivery failed: exit=%v bytes=%d code=%s", err, liveOutput.Len(), code)
	}
	assertLargeMessage(liveOutput.Bytes())
	beforeRevoke, err := store.DeviceByID(ctx, original)
	if err != nil {
		t.Fatal("paired device missing before revocation")
	}
	assertOldTokenDenied := func() {
		t.Helper()
		if _, err := store.ActiveDeviceByAuthHash(ctx, beforeRevoke.AuthHash); !errors.Is(err, storeerrors.ErrNotFound) {
			t.Fatal("old transport credential retained access")
		}
	}
	invoke("", "device", "revoke", "--yes")
	assertOldTokenDenied()
	if count, err := store.CountActiveGrants(ctx, original); err != nil || count != 0 {
		t.Fatal("revocation retained an active grant")
	}
	if _, err := os.Stat(filepath.Join(state, "regional.json")); !os.IsNotExist(err) {
		t.Fatal("revocation retained local pairing configuration")
	}
	if _, err := os.Stat(filepath.Join(state, "cache.sqlite3")); !os.IsNotExist(err) {
		t.Fatal("revocation retained the local session cache")
	}
	if err := waitFor(t, 3*time.Second, func() bool {
		if _, err := io.WriteString(input, "count\n"); err != nil {
			return false
		}
		return scan.Scan() && scan.Text() == "peers:0"
	}); err != nil {
		t.Fatal("revocation retained the host peer")
	}
	if pair() != original {
		t.Fatal("revocation replaced long-term identity")
	}
	assertOldTokenDenied()
	afterRepair, err := store.DeviceByID(ctx, original)
	if err != nil || beforeRevoke.PublicKeyFingerprint != afterRepair.PublicKeyFingerprint || bytes.Equal(beforeRevoke.AuthHash, afterRepair.AuthHash) {
		t.Fatal("re-pairing changed key identity or reused revoked credentials")
	}
	if err := os.Remove(filepath.Join(state, "regional.json")); err != nil {
		t.Fatal(err)
	}
	if pair() != original {
		t.Fatal("ordinary config loss replaced identity")
	}
	if _, err := io.WriteString(input, "count\n"); err != nil {
		t.Fatal(err)
	}
	if !scan.Scan() || scan.Text() != "peers:1" {
		t.Fatal("recovery left duplicate active peer entries")
	}
	if count, err := store.CountActiveGrants(ctx, original); err != nil || count != 1 {
		t.Fatal("recovery duplicated or lost its grant")
	}
	invoke("", "device", "revoke", "--yes")
	invoke("", "device", "forget", "--yes")
	if pair() == original {
		t.Fatal("explicit forget retained the previous device identity")
	}
	invoke("", "device", "revoke", "--yes")
	invoke("", "device", "forget", "--yes")
}

func nativeFixtureKeyring(t *testing.T, root string) []string {
	t.Helper()
	const fixture = `import json, sys
from pathlib import Path
from client_environment import isolated_keyring
with isolated_keyring(Path(sys.argv[1])) as env:
    print(json.dumps(env), flush=True)
    sys.stdin.read()
`
	child := exec.Command("python3", "-c", fixture, t.TempDir())
	child.Dir = filepath.Join(root, "tests", "integration")
	child.Stderr = io.Discard
	hold, err := child.StdinPipe()
	if err != nil {
		t.Fatal(err)
	}
	output, err := child.StdoutPipe()
	if err != nil {
		t.Fatal(err)
	}
	if err := child.Start(); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = hold.Close(); _ = child.Wait() })
	var values map[string]string
	if err := json.NewDecoder(output).Decode(&values); err != nil ||
		!strings.HasPrefix(values["DBUS_SESSION_BUS_ADDRESS"], "unix:abstract=whalerein-") {
		t.Fatal("isolated Secret Service unavailable")
	}
	env := os.Environ()
	for key, value := range values {
		env = append(env, key+"="+value)
	}
	return env
}

// cliErrorCode extracts only the bounded error code from CLI JSON output.
func cliErrorCode(output []byte) string {
	for _, line := range bytes.Split(output, []byte("\n")) {
		var outcome struct {
			Error *struct {
				Code string `json:"code"`
			} `json:"error"`
		}
		if json.Unmarshal(line, &outcome) == nil && outcome.Error != nil {
			return outcome.Error.Code
		}
	}
	return ""
}

// bounded keeps a diagnostic stderr excerpt free of long payloads.
