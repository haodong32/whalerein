package store_test

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"path/filepath"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

func openSQLite(t *testing.T) (*store.Store, context.Context) {
	t.Helper()
	ctx := context.Background()
	path := filepath.Join(t.TempDir(), "access.db")
	data, err := store.Open(ctx, path)
	if err != nil {
		t.Fatalf("open sqlite: %v", err)
	}
	t.Cleanup(data.Close)
	if err := data.Migrate(ctx); err != nil {
		t.Fatalf("migrate sqlite: %v", err)
	}
	return data, ctx
}

func TestSQLiteFreshInstallAndIdempotentMigration(t *testing.T) {
	data, ctx := openSQLite(t)
	if err := data.Migrate(ctx); err != nil {
		t.Fatalf("second migrate: %v", err)
	}
	if _, err := data.EnsureHost(ctx, "host"); err != nil {
		t.Fatalf("ensure host: %v", err)
	}
	if _, err := data.EnsureHost(ctx, "host"); err != nil {
		t.Fatalf("ensure host twice: %v", err)
	}
	host, err := data.GetHost(ctx, "host")
	if err != nil {
		t.Fatalf("get host: %v", err)
	}
	if host.HostID != "host" || host.CreatedAt.IsZero() {
		t.Fatalf("unexpected host: %#v", host)
	}
}

func TestSQLiteRequestRoundTripPreservesTimestampsAndBytes(t *testing.T) {
	data, ctx := openSQLite(t)
	now := time.Now().UTC().Truncate(time.Millisecond)
	if err := data.CreatePairedDevice(ctx, "host", "device", "fingerprint", []byte("auth"), now); err != nil {
		t.Fatal(err)
	}
	digest := make([]byte, 32)
	for i := range digest {
		digest[i] = byte(i)
	}
	request := store.Request{
		RequestID: "request-1", HostID: "host", DeviceID: "device",
		CommandID: "command-1", PayloadDigest: digest, Kind: 3,
		ProtocolVersion: 1, RelayVersion: 5, Status: whalereinv1.OnlineStatus_received,
		RequestBody: []byte{1, 2, 3, 0, 255},
		ExpiresAt:   now.Add(time.Hour), TraceID: "trace", CreatedAt: now, UpdatedAt: now,
		Stamp: &store.GrantStamp{ResultLimitBytes: 1 << 20},
	}
	if _, err := data.CreateRequest(ctx, request); err != nil {
		t.Fatalf("create request: %v", err)
	}
	loaded, err := data.GetRequest(ctx, "request-1")
	if err != nil {
		t.Fatalf("get request: %v", err)
	}
	if string(loaded.RequestBody) != string(request.RequestBody) {
		t.Fatalf("body changed: %v", loaded.RequestBody)
	}
	if !loaded.ExpiresAt.Equal(request.ExpiresAt) {
		t.Fatalf("expiry = %s, want %s", loaded.ExpiresAt, request.ExpiresAt)
	}
	if !loaded.CreatedAt.Equal(now) {
		t.Fatalf("created = %s, want %s", loaded.CreatedAt, now)
	}
}

func TestSQLiteRetainsUniqueCommandIdentity(t *testing.T) {
	data, ctx := openSQLite(t)
	now := time.Now().UTC()
	if err := data.CreatePairedDevice(ctx, "host", "device", "fingerprint", []byte("auth2"), now); err != nil {
		t.Fatal(err)
	}
	request := store.Request{
		RequestID: "request-1", HostID: "host", DeviceID: "device",
		CommandID: "same-command", Kind: 3, ProtocolVersion: 1, RelayVersion: 5,
		Status: whalereinv1.OnlineStatus_received, ExpiresAt: now.Add(time.Hour),
		CreatedAt: now, UpdatedAt: now,
		Stamp: &store.GrantStamp{ResultLimitBytes: 1 << 20},
	}
	if _, err := data.CreateRequest(ctx, request); err != nil {
		t.Fatalf("first create: %v", err)
	}
	duplicate := request
	duplicate.RequestID = "request-2"
	outcome, err := data.CreateRequest(ctx, duplicate)
	if err != nil {
		t.Fatalf("duplicate command identity: %v", err)
	}
	if outcome.Created || outcome.Request.RequestID != "request-1" {
		t.Fatalf("duplicate command identity created a second row: %#v", outcome)
	}
}

// openSQLiteAt opens one node-local database at a caller-owned path so a test
// can close and reopen it to prove durable recovery.
func openSQLiteAt(t *testing.T, path string) *store.Store {
	t.Helper()
	data, err := store.Open(context.Background(), path)
	if err != nil {
		t.Fatalf("open sqlite: %v", err)
	}
	if err := data.Migrate(context.Background()); err != nil {
		data.Close()
		t.Fatalf("migrate sqlite: %v", err)
	}
	return data
}

// committedPairingFixture writes the fenced HELLO/FINISH and the COMMIT outbox
// row expected by CommitPairing on the SQLite backend.
func sqliteCommittedPairingFixture(t *testing.T, data *store.Store, ctx context.Context, hostID, deviceID, claimID string, at time.Time) (string, store.PairingOperation, *whalereinv1.HostRequest) {
	t.Helper()
	deviceGeneration, err := data.DeviceGeneration(ctx, deviceID)
	if err != nil {
		t.Fatal(err)
	}
	hostGeneration, err := data.HostGeneration(ctx, hostID, deviceID)
	if err != nil {
		t.Fatal(err)
	}
	publicKey := bytes.Repeat([]byte{3}, 32)
	hash := sha256.Sum256(publicKey)
	operation := store.PairingOperation{
		ClaimID: claimID, TokenHash: bytes.Repeat([]byte{9}, 32),
		HostID: hostID, DeviceID: deviceID,
		HelloHash: bytes.Repeat([]byte{1}, 32), HelloResponse: []byte("fixture"),
		ExpiresAt: at.Add(time.Minute), DeviceGeneration: deviceGeneration, HostGeneration: hostGeneration,
		FinishHash: bytes.Repeat([]byte{2}, 32),
	}
	if err := data.CreatePairingToken(ctx, operation.TokenHash, hostID, at.Add(time.Minute), at); err != nil {
		t.Fatal(err)
	}
	if err := data.SavePairingOperation(ctx, operation); err != nil {
		t.Fatal(err)
	}
	operation.FinishResponse, _ = proto.Marshal(&whalereinv1.PairingClaimResponse{
		ClaimId: claimID, HostId: hostID, DeviceId: deviceID, Phase: whalereinv1.PairingPhase_PAIRING_PHASE_FINISH,
		DevicePublicKey: publicKey, ProtocolVersion: 1,
	})
	notice := &whalereinv1.HostRequest{
		RequestId: "fixture-" + claimID, HostId: hostID, DeviceId: deviceID,
		Kind: whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE, ProtocolVersion: 1,
		ExpiresAtUnixMs: operation.ExpiresAt.UnixMilli(),
	}
	notice.Body, _ = proto.Marshal(&whalereinv1.PairingClaim{
		ClaimId: claimID, DeviceId: deviceID, DevicePublicKey: publicKey,
		Phase: whalereinv1.PairingPhase_PAIRING_PHASE_COMMIT, ExpiresAtUnixMs: notice.ExpiresAtUnixMs,
	})
	return hex.EncodeToString(hash[:]), operation, notice
}

// The committed Host grant ID is durable pairing-outcome evidence on the
// runtime SQLite backend. It survives a restart, and a later confirmation
// without a grant never clears it.
func TestSQLitePairingOutcomePersistsHostGrantIDAcrossRestart(t *testing.T) {
	ctx := context.Background()
	now := time.Now().UTC().Truncate(time.Millisecond)
	path := filepath.Join(t.TempDir(), "access.db")
	data := openSQLiteAt(t, path)
	if _, err := data.EnsureHost(ctx, "host"); err != nil {
		t.Fatal(err)
	}
	fingerprint, operation, notice := sqliteCommittedPairingFixture(t, data, ctx, "host", "device", "claim", now)
	if accepted, err := data.CommitPairing(ctx, operation.TokenHash, "host", "device", fingerprint, bytes.Repeat([]byte{4}, 32), now, notice, &operation); err != nil || !accepted {
		t.Fatalf("commit = %v %v", accepted, err)
	}
	confirmed, err := data.ConfirmPairingHost(ctx, "claim", "host", "device", notice.RequestId, "grant_sqlite_persisted", now)
	if err != nil || !confirmed {
		t.Fatalf("confirm = %v %v", confirmed, err)
	}
	outcome, err := data.PairingOutcome(ctx, "claim", "device")
	if err != nil || !outcome.HostConfirmed || outcome.GrantID != "grant_sqlite_persisted" {
		t.Fatalf("outcome = %#v err = %v", outcome, err)
	}

	// A later confirmation that does not name a grant must not clear the value.
	if confirmed, err := data.ConfirmPairingHost(ctx, "claim", "host", "device", notice.RequestId, "", now); err != nil || !confirmed {
		t.Fatalf("repeat confirm = %v %v", confirmed, err)
	}
	if outcome, err := data.PairingOutcome(ctx, "claim", "device"); err != nil || outcome.GrantID != "grant_sqlite_persisted" {
		t.Fatalf("grant was cleared: %#v err = %v", outcome, err)
	}

	// Restart: the durable outcome is the retry path and keeps the same ID.
	data.Close()
	reopened := openSQLiteAt(t, path)
	defer reopened.Close()
	outcome, err = reopened.PairingOutcome(ctx, "claim", "device")
	if err != nil || outcome.GrantID != "grant_sqlite_persisted" {
		t.Fatalf("grant after restart = %#v err = %v", outcome, err)
	}
}
