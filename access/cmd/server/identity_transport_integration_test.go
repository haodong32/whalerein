//go:build integration

package main

// Opt-in full stack: actual CLI, SQL grants, Noise pairing and encrypted plugin
// results over the isolated 1 MiB broker. It never reads user credentials.
import (
	"bufio"
	"bytes"
	"context"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/config"
	"github.com/whalerein/whalerein/access/internal/store/storetest"
)

// When the CLI pairs, exact large encrypted Messages arrive; revocation removes access,
// while explicit re-pairing retains key identity without duplicate grants or peers.
func TestIdentityAndEncryptedTransportAgainstIsolatedStack(t *testing.T) {
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
	secrets := t.TempDir()
	binary := filepath.Join(root, "target/debug/whalerein")
	invoke := func(args ...string) []byte {
		command := exec.CommandContext(ctx, binary, append([]string{"--json", "--yes", "--data-dir", data}, args...)...)
		command.Env = append(os.Environ(), "WHALEREIN_SECRET_BACKEND=file:"+secrets, "WHALEREIN_LOG=error")
		var stdout, stderr bytes.Buffer
		command.Stdout = &stdout
		command.Stderr = &stderr
		if err := command.Run(); err != nil {
			t.Fatalf("isolated CLI %s failed: code=%s stderr=%s", args[0], cliErrorCode(stdout.Bytes()), bounded(stderr.String()))
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
		if json.Unmarshal(invoke("pair", invite()), &result) != nil || result.Data.DeviceID == "" {
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
	// Snapshot history is installed by an explicit sync (a watch attach is not
	// replayed as new output). This still exercises 2 MiB snapshot decryption
	// and cache staging; the live tail below proves delivery of the new turn.
	sync := invoke("sync", "--session", "session-flow")
	var syncOutcome struct {
		Data struct {
			Events int `json:"events"`
		} `json:"data"`
	}
	if json.Unmarshal(sync, &syncOutcome) != nil || syncOutcome.Data.Events == 0 {
		t.Fatal("large encrypted snapshot was not synchronized")
	}
	live := exec.CommandContext(ctx, binary, "--json", "--data-dir", data, "tail", "--session", "session-flow", "--once", "--timeout", "15")
	live.Env = append(os.Environ(), "WHALEREIN_SECRET_BACKEND=file:"+secrets, "WHALEREIN_LOG=info")
	var liveOutput bytes.Buffer
	live.Stdout = &liveOutput
	liveError, err := live.StderrPipe()
	if err != nil {
		t.Fatal(err)
	}
	if err := live.Start(); err != nil {
		t.Fatal(err)
	}
	liveScan := bufio.NewScanner(liveError)
	subscribed := false
	for liveScan.Scan() {
		if strings.Contains(liveScan.Text(), "watch_ready") {
			subscribed = true
			break
		}
	}
	if !subscribed {
		_ = live.Process.Kill()
		_ = live.Wait()
		t.Fatal("live CLI subscription unavailable")
	}
	go func() { _, _ = io.Copy(io.Discard, liveError) }()
	if _, err := io.WriteString(input, "emit\n"); err != nil {
		t.Fatal(err)
	}
	if err := live.Wait(); err != nil || liveOutput.Len() < 128*1024 {
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
	tokenPath := filepath.Join(secrets, hex.EncodeToString([]byte("whalerein:v1:device-token:"+original)))
	oldToken, err := os.ReadFile(tokenPath)
	if err != nil {
		t.Fatal("paired transport credential unavailable")
	}
	assertOldTokenDenied := func() {
		t.Helper()
		request, err := http.NewRequestWithContext(ctx, http.MethodGet, base+"/v1/hosts", nil)
		if err != nil {
			t.Fatal("revocation probe could not be constructed")
		}
		request.Header.Set("Authorization", "Bearer "+string(oldToken))
		response, err := http.DefaultClient.Do(request)
		if err != nil {
			t.Fatal("revocation probe could not reach Access")
		}
		response.Body.Close()
		if response.StatusCode != http.StatusUnauthorized && response.StatusCode != http.StatusForbidden {
			t.Fatal("old transport credential retained access")
		}
	}
	invoke("revoke")
	assertOldTokenDenied()
	if count, err := store.CountActiveGrants(ctx, original); err != nil || count != 0 {
		t.Fatal("revocation retained an active grant")
	}
	if _, err := os.Stat(filepath.Join(data, "config.json")); !os.IsNotExist(err) {
		t.Fatal("revocation retained local pairing configuration")
	}
	if _, err := os.Stat(filepath.Join(data, "cache.sqlite3")); !os.IsNotExist(err) {
		t.Fatal("revocation retained the local session cache")
	}
	if _, err := os.Stat(tokenPath); !os.IsNotExist(err) {
		t.Fatal("revocation retained the local transport token")
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
	if err := os.Remove(filepath.Join(data, "config.json")); err != nil {
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
	invoke("revoke")
	invoke("forget")
	if entries, err := os.ReadDir(secrets); err != nil || len(entries) != 0 {
		t.Fatal("explicit forget left identity credentials")
	}
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
func bounded(value string) string {
	if len(value) > 2000 {
		return value[len(value)-2000:]
	}
	return value
}
