//go:build integration

package store_test

import (
	"bytes"
	"context"
	"crypto/sha256"
	"embed"
	"encoding/hex"
	"errors"
	"fmt"
	"google.golang.org/protobuf/proto"
	"strings"
	"testing"
	"time"

	"github.com/jackc/pgx/v5/pgxpool"
	"github.com/whalerein/whalerein/access/internal/store"
	"github.com/whalerein/whalerein/access/internal/store/storetest"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

//go:embed migrations/*.sql
var embeddedMigrations embed.FS

// When migrations run on an empty schema, PostgreSQL contains versioned application tables and
// the required indexes.
func TestMigrateCreatesVersionedTablesAndIndexes(t *testing.T) {
	s, db := storetest.New(t)
	ctx := context.Background()

	rows, err := s.Pool().Query(ctx, `
		SELECT table_name FROM information_schema.tables
		WHERE table_schema = current_schema() AND table_type = 'BASE TABLE'
		ORDER BY table_name`)
	if err != nil {
		t.Fatalf("list tables: %v", err)
	}
	defer rows.Close()
	var tables []string
	for rows.Next() {
		var name string
		if err := rows.Scan(&name); err != nil {
			t.Fatal(err)
		}
		tables = append(tables, name)
	}
	if err := rows.Err(); err != nil {
		t.Fatal(err)
	}
	want := []string{"device_credentials", "device_sessions", "devices", "external_device_revocations", "external_enrollments", "external_operations", "external_removals", "grants", "host_proof_nonces", "host_signature_replays", "hosts", "pairing_device_sessions", "pairing_operations", "pairing_outcomes", "pairing_tokens", "requests", "schema_migrations", "temporary_ciphertext_leases", "temporary_ciphertext_objects"}
	if len(tables) != len(want) {
		t.Fatalf("tables = %v, want exactly %v", tables, want)
	}
	for i := range want {
		if tables[i] != want[i] {
			t.Fatalf("tables = %v, want %v", tables, want)
		}
	}

	indexes := map[string]string{}
	rows, err = s.Pool().Query(ctx, `
		SELECT indexname, indexdef FROM pg_indexes
		WHERE schemaname = current_schema()`)
	if err != nil {
		t.Fatalf("list indexes: %v", err)
	}
	defer rows.Close()
	for rows.Next() {
		var name, def string
		if err := rows.Scan(&name, &def); err != nil {
			t.Fatal(err)
		}
		indexes[name] = def
	}
	for _, required := range []string{
		"device_credentials_pkey", "device_sessions_pkey", "requests_pkey", "requests_command_unique",
		"requests_forward_due_idx", "requests_device_session_live", "grants_pending_revocation_idx",
		"external_enrollments_workspace", "external_operations_authorization_id_key",
		"host_proof_nonces_created_at", "external_device_revocations_pkey",
		"cloud_pairing_outcomes_grant_idx", "cloud_pairing_outcomes_auth_idx",
		"pairing_device_sessions_pkey",
		"temporary_ciphertext_objects_pkey", "temporary_ciphertext_objects_expiry_idx",
		"temporary_ciphertext_objects_cleanup_idx", "temporary_ciphertext_objects_active_upload_idx",
		"temporary_ciphertext_leases_object_idx",
	} {
		if _, ok := indexes[required]; !ok {
			t.Errorf("missing index %s; have %v", required, indexes)
		}
	}
	if got := indexes["requests_command_unique"]; got == "" || !containsAll(got, "UNIQUE", "command_id") {
		t.Errorf("requests_command_unique = %q", got)
	}
	if got := indexes["requests_forward_due_idx"]; got == "" || !containsAll(got, "forward_state", "pending") {
		t.Errorf("requests_forward_due_idx = %q", got)
	}
	if got := indexes["grants_pending_revocation_idx"]; got == "" || !containsAll(got, "revoked_at", "replayed_at") {
		t.Errorf("grants_pending_revocation_idx = %q", got)
	}
	if db.Schema == "" {
		t.Fatal("schema not set")
	}
}

func containsAll(haystack string, needles ...string) bool {
	for _, needle := range needles {
		if !strings.Contains(haystack, needle) {
			return false
		}
	}
	return true
}

// When hosts and devices are paired, grants and pinned host keys remain consistent and credential
// conflicts are rejected.
func TestHostDeviceAndGrantStoresPreserveRelationships(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()

	host, err := s.EnsureHost(ctx, "h1")
	if err != nil {
		t.Fatal(err)
	}
	if host.HostID != "h1" {
		t.Fatalf("host = %+v", host)
	}
	if _, err := s.EnsureHost(ctx, "h1"); err != nil {
		t.Fatal(err)
	}
	key := bytes.Repeat([]byte{9}, 32)
	if err := s.SetHostAuthorizationKey(ctx, "h1", key); err != nil {
		t.Fatal(err)
	}
	if err := s.SetHostAuthorizationKey(ctx, "h1", bytes.Repeat([]byte{8}, 32)); err != nil {
		t.Fatal(err)
	}
	gotKey, err := s.HostAuthorizationKey(ctx, "h1")
	if err != nil || !bytes.Equal(gotKey, key) {
		t.Fatalf("authorization key = %x err %v", gotKey, err)
	}
	if _, err := s.HostAuthorizationKey(ctx, "missing"); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("missing key err = %v", err)
	}
	if err := s.SetHostAuthorizationKey(ctx, "", key); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("empty host err = %v", err)
	}
	if err := s.SetHostAuthorizationKey(ctx, "h1", []byte("short")); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("short key err = %v", err)
	}
	if err := s.SetHostAuthorizationKey(ctx, "missing", key); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("missing host key err = %v", err)
	}
	if _, err := s.EnsureHost(ctx, "h-nokey"); err != nil {
		t.Fatal(err)
	}
	if _, err := s.HostAuthorizationKey(ctx, "h-nokey"); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("unset key err = %v", err)
	}
	if _, err := s.GetHost(ctx, "missing"); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("missing host err = %v", err)
	}
	if _, err := s.DeviceByID(ctx, "d1"); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("missing device err = %v", err)
	}
	if err := s.UpdateHostMetadata(ctx, "h1", "fp-a", 1, time.Now()); err != nil {
		t.Fatal(err)
	}
	host, _ = s.GetHost(ctx, "h1")
	if host.Fingerprint != "fp-a" || host.ProtocolVersion != 1 {
		t.Fatalf("host metadata = %+v", host)
	}
	if err := s.UpdateHostMetadata(ctx, "missing", "fp", 1, time.Now()); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("metadata missing err = %v", err)
	}

	hash := []byte("auth-d1")
	if err := s.CreatePairedDevice(ctx, "h1", "d1", "device-fp", hash, time.Now()); err != nil {
		t.Fatal(err)
	}
	device, err := s.ActiveDeviceByAuthHash(ctx, hash)
	if err != nil {
		t.Fatal(err)
	}
	if device.DeviceID != "d1" || device.PublicKeyFingerprint != "device-fp" {
		t.Fatalf("device = %+v", device)
	}
	if device, err := s.DeviceByID(ctx, "d1"); err != nil || device.DeviceID != "d1" {
		t.Fatalf("device by id = %+v err=%v", device, err)
	}
	if _, err := s.ActiveDeviceByAuthHash(ctx, []byte("wrong")); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("wrong hash err = %v", err)
	}
	if active, err := s.HasActiveGrant(ctx, "h1", "d1"); err != nil || !active {
		t.Fatalf("active = %v err=%v", active, err)
	}
	if active, err := s.HasActiveGrant(ctx, "h1", "d2"); err != nil || active {
		t.Fatalf("unexpected active grant")
	}
	hosts, err := s.ListAuthorizedHosts(ctx, "d1")
	if err != nil || len(hosts) != 1 || hosts[0].HostID != "h1" {
		t.Fatalf("hosts = %+v err=%v", hosts, err)
	}
	if count, err := s.CountActiveGrants(ctx, "d1"); err != nil || count != 1 {
		t.Fatalf("count = %d err=%v", count, err)
	}
	if err := s.CreatePairedDevice(ctx, "h1", "d1", "device-fp", []byte("auth-d1-rotated"), time.Now()); err != nil {
		t.Fatal(err)
	}
	if _, err := s.ActiveDeviceByAuthHash(ctx, []byte("auth-d1")); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("old hash should fail: %v", err)
	}
	if _, err := s.ActiveDeviceByAuthHash(ctx, []byte("auth-d1-rotated")); err != nil {
		t.Fatalf("new hash: %v", err)
	}
	// Unique auth_hash across devices.
	if err := s.CreatePairedDevice(ctx, "h1", "d2", "fp", []byte("auth-d1-rotated"), time.Now()); !errors.Is(err, store.ErrConflict) {
		t.Fatalf("duplicate auth hash err = %v", err)
	}
}

// When grants are revoked, authorization is removed and pending revocations remain available for
// replay until acknowledged.
func TestRevocationStoreUpdatesAndReplaysPendingGrants(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	now := time.Now().UTC()
	if _, err := s.EnsureHost(ctx, "h1"); err != nil {
		t.Fatal(err)
	}
	if _, err := s.EnsureHost(ctx, "h2"); err != nil {
		t.Fatal(err)
	}
	hash := []byte("auth-d1")
	if err := s.CreatePairedDevice(ctx, "h1", "d1", "fp", hash, now); err != nil {
		t.Fatal(err)
	}
	if err := s.CreatePairedDevice(ctx, "h2", "d1", "fp", hash, now); err != nil {
		t.Fatal(err)
	}

	if err := s.RevokeGrant(ctx, "h1", "d1", now); err != nil {
		t.Fatal(err)
	}
	if err := s.RevokeGrant(ctx, "h1", "d1", now); err != nil {
		t.Fatalf("second revoke should be idempotent: %v", err)
	}
	if err := s.RevokeGrant(ctx, "h1", "missing", now); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("missing grant err = %v", err)
	}
	pending, err := s.PendingRevocations(ctx, "h1")
	if err != nil || len(pending) != 1 || pending[0].DeviceID != "d1" {
		t.Fatalf("pending = %+v err=%v", pending, err)
	}
	revokedAt := pending[0].RevokedAt
	if err := s.MarkRevocationsReplayed(ctx, "h1", []string{"d1"}, revokedAt, now.Add(time.Second), map[string]uint64{"d1": 1}); err != nil {
		t.Fatal(err)
	}
	pending, _ = s.PendingRevocations(ctx, "h1")
	if len(pending) != 0 {
		t.Fatalf("pending after ack = %+v", pending)
	}
	if err := s.MarkRevocationsReplayed(ctx, "h1", nil, revokedAt, now, nil); err != nil {
		t.Fatal(err)
	}

	// Re-pairing clears the old revocation for that grant.
	if err := s.RevokeGrant(ctx, "h1", "d1", now); err != nil {
		t.Fatal(err)
	}
	if err := s.CreatePairedDevice(ctx, "h1", "d1", "fp", hash, now); err != nil {
		t.Fatal(err)
	}
	pending, _ = s.PendingRevocations(ctx, "h1")
	if len(pending) != 0 {
		t.Fatalf("pending after re-pair = %+v", pending)
	}

	hosts, exists, err := s.RevokeDeviceGrants(ctx, "d1", now.Add(time.Minute))
	if err != nil || !exists {
		t.Fatalf("revoke device = %v exists=%v err=%v", hosts, exists, err)
	}
	if len(hosts) != 2 || hosts[0] != "h1" || hosts[1] != "h2" {
		t.Fatalf("hosts = %v", hosts)
	}
	if _, err := s.ActiveDeviceByAuthHash(ctx, hash); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("revoked device auth err = %v", err)
	}
	if count, _ := s.CountActiveGrants(ctx, "d1"); count != 0 {
		t.Fatalf("active grants = %d", count)
	}
	hosts, exists, err = s.RevokeDeviceGrants(ctx, "d1", now)
	if err != nil || !exists || len(hosts) != 0 {
		t.Fatalf("second device revoke = %v exists=%v err=%v", hosts, exists, err)
	}
	hosts, exists, err = s.RevokeDeviceGrants(ctx, "missing", now)
	if err != nil || exists || len(hosts) != 0 {
		t.Fatalf("missing device revoke = %v exists=%v err=%v", hosts, exists, err)
	}
}

// When pairing tokens are created, used and expired, usage is one-shot and only expired tokens are
// deleted.
func TestPairingTokenStoreCreatesConsumesAndExpiresTokens(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	now := time.Now().UTC()
	if _, err := s.EnsureHost(ctx, "h1"); err != nil {
		t.Fatal(err)
	}
	hash := []byte("token-hash-1")
	if err := s.CreatePairingToken(ctx, hash, "h1", now.Add(time.Minute), now); err != nil {
		t.Fatal(err)
	}
	token, err := s.PairingTokenByHash(ctx, hash)
	if err != nil {
		t.Fatal(err)
	}
	if token.HostID != "h1" || token.UsedAt != nil || token.FailureCount != 0 {
		t.Fatalf("token = %+v", token)
	}
	if _, err := s.PairingTokenByHash(ctx, []byte("missing")); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("missing token err = %v", err)
	}
	count, err := s.IncrementPairingFailure(ctx, hash, now)
	if err != nil || count != 1 {
		t.Fatalf("failure count = %d err=%v", count, err)
	}
	count, _ = s.IncrementPairingFailure(ctx, hash, now)
	if count != 2 {
		t.Fatalf("failure count = %d", count)
	}
	if _, err := s.IncrementPairingFailure(ctx, []byte("missing"), now); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("missing failure err = %v", err)
	}
	used, err := s.MarkPairingTokenUsed(ctx, hash, now)
	if err != nil || !used {
		t.Fatalf("used = %v err=%v", used, err)
	}
	used, err = s.MarkPairingTokenUsed(ctx, hash, now)
	if err != nil || used {
		t.Fatalf("second used = %v err=%v", used, err)
	}
	token, _ = s.PairingTokenByHash(ctx, hash)
	if token.UsedAt == nil {
		t.Fatal("used_at not retained")
	}
	if deleted, err := s.DeleteExpiredPairingTokens(ctx, now.Add(-time.Minute)); err != nil || deleted != 0 {
		t.Fatalf("early delete = %d err=%v", deleted, err)
	}
	if deleted, err := s.DeleteExpiredPairingTokens(ctx, now.Add(2*time.Minute)); err != nil || deleted != 1 {
		t.Fatalf("delete = %d err=%v", deleted, err)
	}
	if err := s.CreatePairingToken(ctx, hash, "h1", now.Add(time.Minute), now); err != nil {
		t.Fatal(err)
	}
	if err := s.CreatePairingToken(ctx, hash, "h1", now.Add(time.Minute), now); !errors.Is(err, store.ErrConflict) {
		t.Fatalf("duplicate token err = %v", err)
	}
	if err := s.CreatePairingToken(ctx, []byte("bad-host"), "missing", now.Add(time.Minute), now); err == nil {
		t.Fatal("expected foreign key error")
	}
}

func seedDevice(t *testing.T, s *store.Store, hostID, deviceID string) {
	t.Helper()
	ctx := context.Background()
	if _, err := s.EnsureHost(ctx, hostID); err != nil {
		t.Fatal(err)
	}
	if err := s.CreatePairedDevice(ctx, hostID, deviceID, "fp", []byte("auth-"+deviceID), time.Now().UTC()); err != nil {
		t.Fatal(err)
	}
}

// seedLegacyDevice writes the pre-generation grant shape for migration tests.
func seedLegacyDevice(t *testing.T, s *store.Store, hostID, deviceID string) {
	t.Helper()
	ctx := context.Background()
	now := time.Now().UTC()
	if _, err := s.EnsureHost(ctx, hostID); err != nil {
		t.Fatal(err)
	}
	if _, err := s.Pool().Exec(ctx, `INSERT INTO devices(device_id,public_key_fingerprint,created_at,updated_at)
		VALUES($1,'fp',$2,$2)`, deviceID, now); err != nil {
		t.Fatal(err)
	}
	if _, err := s.Pool().Exec(ctx, `INSERT INTO device_credentials(auth_hash,device_id,created_at)
		VALUES($1,$2,$3)`, []byte("auth-"+deviceID), deviceID, now); err != nil {
		t.Fatal(err)
	}
	if _, err := s.Pool().Exec(ctx, `INSERT INTO grants(host_id,device_id,created_at,updated_at,grant_generation)
		VALUES($1,$2,$3,$3,1)`, hostID, deviceID, now); err != nil {
		t.Fatal(err)
	}
}

func newRequest(id, hostID, deviceID, commandID string, digest []byte, status whalereinv1.OnlineStatus, forward store.ForwardState, now time.Time) store.Request {
	return store.Request{
		RequestID:       id,
		HostID:          hostID,
		DeviceID:        deviceID,
		CommandID:       commandID,
		PayloadDigest:   digest,
		Kind:            whalereinv1.RequestKind_REQUEST_KIND_COMMAND,
		ProtocolVersion: 1,
		Status:          status,
		RequestBody:     []byte("body-" + id),
		ExpiresAt:       now.Add(time.Hour),
		ForwardState:    forward,
		ForwardAfter:    now,
		CreatedAt:       now,
		UpdatedAt:       now,
		Stamp: &store.GrantStamp{
			GrantGeneration:            1,
			RevocationGeneration:       1,
			DeviceRevocationGeneration: 1,
			ResultLimitBytes:           16 << 20,
		},
	}
}

// When a command ID is reused, matching digests return the existing request and changed digests
// are rejected.
func TestCreateRequestReusesMatchingCommandAndRejectsDigestConflicts(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	now := time.Now().UTC()
	seedDevice(t, s, "h1", "d1")
	digest := []byte("digest-1")

	row := newRequest("r1", "h1", "d1", "c1", digest, whalereinv1.OnlineStatus_received, store.ForwardPending, now)
	outcome, err := s.CreateRequest(ctx, row)
	if err != nil || !outcome.Created || outcome.Request.RequestID != "r1" {
		t.Fatalf("create = %+v err=%v", outcome, err)
	}
	outcome, err = s.CreateRequest(ctx, row)
	if err != nil || outcome.Created || outcome.DigestMismatch || outcome.Request.RequestID != "r1" {
		t.Fatalf("dedupe = %+v err=%v", outcome, err)
	}
	other := row
	other.RequestID = "r2"
	other.PayloadDigest = []byte("digest-2")
	outcome, err = s.CreateRequest(ctx, other)
	if err != nil || outcome.Created || !outcome.DigestMismatch || outcome.Request.RequestID != "r1" {
		t.Fatalf("digest mismatch = %+v err=%v", outcome, err)
	}
	if count, _ := s.CountRequests(ctx); count != 1 {
		t.Fatalf("request count = %d", count)
	}
	// A handshake with the same request id conflicts.
	handshake := newRequest("r1", "h1", "d1", "", nil, whalereinv1.OnlineStatus_received, store.ForwardPending, now)
	handshake.Kind = whalereinv1.RequestKind_REQUEST_KIND_HANDSHAKE
	if _, err := s.CreateRequest(ctx, handshake); !errors.Is(err, store.ErrConflict) {
		t.Fatalf("duplicate request id err = %v", err)
	}
	// Handshakes have no command unique constraint.
	for i, id := range []string{"h1", "h2"} {
		req := newRequest(id, "h1", "d1", "", nil, whalereinv1.OnlineStatus_received, store.ForwardPending, now)
		req.Kind = whalereinv1.RequestKind_REQUEST_KIND_HANDSHAKE
		if _, err := s.CreateRequest(ctx, req); err != nil {
			t.Fatalf("handshake %d: %v", i, err)
		}
	}
	invalid := row
	invalid.RequestID = "bad"
	invalid.Status = whalereinv1.OnlineStatus_online_status_unspecified
	if _, err := s.CreateRequest(ctx, invalid); err == nil {
		t.Fatal("expected invalid status error")
	}
	invalid = row
	invalid.RequestID = "bad2"
	invalid.ForwardState = "weird"
	if _, err := s.CreateRequest(ctx, invalid); err == nil {
		t.Fatal("expected invalid forward state error")
	}
	if _, err := s.GetRequest(ctx, "missing"); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("missing request err = %v", err)
	}
	offline := newRequest("offline", "h1", "d1", "c-offline", []byte("d"), whalereinv1.OnlineStatus_host_offline, store.ForwardNone, now)
	outcome, err = s.CreateRequest(ctx, offline)
	if err != nil || !outcome.Created || outcome.Request.Status != whalereinv1.OnlineStatus_host_offline || outcome.Request.ForwardState != store.ForwardNone {
		t.Fatalf("offline = %+v err=%v", outcome, err)
	}
}

// When pending requests become due, workers claim eligible rows with a forwarding lease.
func TestClaimDueRequestsLeasesPendingRows(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	now := time.Now().UTC()
	seedDevice(t, s, "h1", "d1")
	for i := 0; i < 3; i++ {
		row := newRequest(fmt.Sprintf("r%d", i), "h1", "d1", fmt.Sprintf("c%d", i), []byte("d"), whalereinv1.OnlineStatus_received, store.ForwardPending, now)
		if _, err := s.CreateRequest(ctx, row); err != nil {
			t.Fatal(err)
		}
	}
	claimed, err := s.ClaimDueRequests(ctx, now, 2, 10*time.Second)
	if err != nil || len(claimed) != 2 {
		t.Fatalf("claimed = %d err=%v", len(claimed), err)
	}
	for _, row := range claimed {
		if row.ForwardAttempts != 1 || !row.ForwardAfter.After(now) {
			t.Fatalf("lease = %+v", row)
		}
	}
	claimed, _ = s.ClaimDueRequests(ctx, now, 10, time.Minute)
	if len(claimed) != 1 {
		t.Fatalf("remaining = %d", len(claimed))
	}
	claimed, _ = s.ClaimDueRequests(ctx, now.Add(11*time.Second), 10, time.Minute)
	if len(claimed) != 2 {
		t.Fatalf("reclaimed = %d", len(claimed))
	}
	for _, row := range claimed {
		if row.ForwardAttempts != 2 {
			t.Fatalf("attempts = %d", row.ForwardAttempts)
		}
	}
	if rows, err := s.ClaimDueRequests(ctx, now, 0, time.Second); err != nil || rows != nil {
		t.Fatalf("zero limit = %v err=%v", rows, err)
	}
}

// When two concurrent workers each request up to 100 of 30 eligible rows, their
// claims are disjoint and their union is exactly the seeded request set.
func TestClaimDueRequestsConcurrentSkipLocked(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	now := time.Now().UTC()
	seedDevice(t, s, "h1", "d1")
	want := map[string]bool{}
	for i := 0; i < 30; i++ {
		row := newRequest(fmt.Sprintf("r%02d", i), "h1", "d1", fmt.Sprintf("c%02d", i), []byte("d"), whalereinv1.OnlineStatus_received, store.ForwardPending, now)
		want[row.RequestID] = true
		if _, err := s.CreateRequest(ctx, row); err != nil {
			t.Fatal(err)
		}
	}
	type claimResult struct {
		rows []store.Request
		err  error
	}
	first, second := make(chan claimResult, 1), make(chan claimResult, 1)
	start := make(chan struct{})
	go func() {
		<-start
		rows, err := s.ClaimDueRequests(ctx, now, 100, time.Minute)
		first <- claimResult{rows, err}
	}()
	go func() {
		<-start
		rows, err := s.ClaimDueRequests(ctx, now, 100, time.Minute)
		second <- claimResult{rows, err}
	}()
	close(start)
	a, b := <-first, <-second
	if a.err != nil || b.err != nil {
		t.Fatal("concurrent claim worker returned an error")
	}
	seen := map[string]bool{}
	for _, row := range append(a.rows, b.rows...) {
		if !want[row.RequestID] {
			t.Fatal("worker claimed an unseeded request")
		}
		if seen[row.RequestID] {
			t.Fatalf("row %s claimed twice", row.RequestID)
		}
		seen[row.RequestID] = true
	}
	if len(seen) != 30 {
		t.Fatalf("claimed %d rows, want 30", len(seen))
	}
}

// When publication succeeds or exhausts retries, persisted forwarding state distinguishes
// published requests from commands never forwarded.
func TestPublishLifecycleTracksSuccessAndRetryableFailure(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	// PostgreSQL timestamps preserve microseconds, not nanoseconds.
	now := time.Now().UTC().Truncate(time.Microsecond)
	seedDevice(t, s, "h1", "d1")
	row := newRequest("r1", "h1", "d1", "c1", []byte("d"), whalereinv1.OnlineStatus_received, store.ForwardPending, now)
	if _, err := s.CreateRequest(ctx, row); err != nil {
		t.Fatal(err)
	}
	claimed, _ := s.ClaimDueRequests(ctx, now, 10, time.Minute)
	if len(claimed) != 1 {
		t.Fatalf("claimed = %d", len(claimed))
	}
	if err := s.MarkPublished(ctx, "r1", now, 5*time.Second); err != nil {
		t.Fatal(err)
	}
	got, _ := s.GetRequest(ctx, "r1")
	if got.ForwardState != store.ForwardPublished || !got.PublishedOnce || !got.ForwardAfter.Equal(now.Add(5*time.Second)) {
		t.Fatalf("published row = %+v", got)
	}
	if requeued, err := s.RequeueDuePublished(ctx, now, 5); err != nil || requeued != 0 {
		t.Fatalf("early requeue = %d err=%v", requeued, err)
	}
	if requeued, err := s.RequeueDuePublished(ctx, now.Add(6*time.Second), 5); err != nil || requeued != 1 {
		t.Fatalf("requeue = %d err=%v", requeued, err)
	}
	claimed, _ = s.ClaimDueRequests(ctx, now.Add(6*time.Second), 10, time.Minute)
	if len(claimed) != 1 || claimed[0].ForwardAttempts != 2 {
		t.Fatalf("second claim = %+v", claimed)
	}
	if err := s.MarkPublishFailure(ctx, "r1", now.Add(6*time.Second), time.Second, 2); err != nil {
		t.Fatal(err)
	}
	got, _ = s.GetRequest(ctx, "r1")
	if got.Status != whalereinv1.OnlineStatus_received || got.ForwardState != store.ForwardPublished {
		t.Fatalf("published exhaustion row = %+v", got)
	}
	if requeued, _ := s.RequeueDuePublished(ctx, now.Add(time.Hour), 2); requeued != 0 {
		t.Fatalf("exhausted row requeued: %d", requeued)
	}

	// An unconfirmed publish remains unknown when attempts run out.
	other := newRequest("r2", "h1", "d1", "c2", []byte("d"), whalereinv1.OnlineStatus_received, store.ForwardPending, now)
	if _, err := s.CreateRequest(ctx, other); err != nil {
		t.Fatal(err)
	}
	claimed, _ = s.ClaimDueRequests(ctx, now, 10, time.Minute)
	if len(claimed) != 1 || claimed[0].RequestID != "r2" {
		t.Fatalf("other claim = %+v", claimed)
	}
	if err := s.MarkPublishFailure(ctx, "r2", now, time.Second, 1); err != nil {
		t.Fatal(err)
	}
	got, _ = s.GetRequest(ctx, "r2")
	if got.Status != whalereinv1.OnlineStatus_unknown || got.ErrorCode != whalereinv1.ErrorCode_unavailable || got.ForwardState != store.ForwardNone {
		t.Fatalf("uncertain exhaustion row = %+v", got)
	}
}

// When host results arrive, allowed status transitions are persisted and invalid or conflicting
// results are rejected.
func TestApplyHostResultAcceptsAllowedTransitionsAndRejectsInvalidOnes(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	now := time.Now().UTC()
	seedDevice(t, s, "h1", "d1")
	row := newRequest("r1", "h1", "d1", "c1", []byte("d"), whalereinv1.OnlineStatus_received, store.ForwardPending, now)
	if _, err := s.CreateRequest(ctx, row); err != nil {
		t.Fatal(err)
	}
	update := store.HostResultUpdate{RequestID: "r1", HostID: "h1", DeviceID: "d1", Status: whalereinv1.OnlineStatus_host_accepted, At: now}
	applied, err := s.ApplyHostResult(ctx, update)
	if err != nil || !applied {
		t.Fatalf("accepted = %v err=%v", applied, err)
	}
	got, _ := s.GetRequest(ctx, "r1")
	if got.Status != whalereinv1.OnlineStatus_host_accepted || got.ForwardState != store.ForwardNone {
		t.Fatalf("row = %+v", got)
	}
	update.Status = whalereinv1.OnlineStatus_succeeded
	update.ResultBody = []byte("ciphertext")
	update.ChannelID = "channel-1"
	applied, _ = s.ApplyHostResult(ctx, update)
	if !applied {
		t.Fatal("expected terminal transition")
	}
	got, _ = s.GetRequest(ctx, "r1")
	if got.Status != whalereinv1.OnlineStatus_succeeded || !bytes.Equal(got.ResultBody, []byte("ciphertext")) || got.ChannelID != "channel-1" {
		t.Fatalf("terminal row = %+v", got)
	}
	update.Status = whalereinv1.OnlineStatus_failed
	if applied, _ := s.ApplyHostResult(ctx, update); applied {
		t.Fatal("terminal row must not change")
	}

	// unknown -> failed is allowed.
	row2 := newRequest("r2", "h1", "d1", "c2", []byte("d"), whalereinv1.OnlineStatus_received, store.ForwardPending, now)
	_, _ = s.CreateRequest(ctx, row2)
	update = store.HostResultUpdate{RequestID: "r2", HostID: "h1", DeviceID: "d1", Status: whalereinv1.OnlineStatus_unknown, At: now}
	if applied, _ := s.ApplyHostResult(ctx, update); !applied {
		t.Fatal("received -> unknown")
	}
	update.Status = whalereinv1.OnlineStatus_failed
	if applied, _ := s.ApplyHostResult(ctx, update); !applied {
		t.Fatal("unknown -> failed")
	}
	update.Status = whalereinv1.OnlineStatus_succeeded
	if applied, _ := s.ApplyHostResult(ctx, update); applied {
		t.Fatal("failed is final")
	}

	// received -> received is not a valid transition.
	row3 := newRequest("r3", "h1", "d1", "c3", []byte("d"), whalereinv1.OnlineStatus_received, store.ForwardPending, now)
	_, _ = s.CreateRequest(ctx, row3)
	update = store.HostResultUpdate{RequestID: "r3", HostID: "h1", DeviceID: "d1", Status: whalereinv1.OnlineStatus_received, At: now}
	if applied, _ := s.ApplyHostResult(ctx, update); applied {
		t.Fatal("received -> received")
	}
	update.Status = whalereinv1.OnlineStatus_rejected
	if applied, _ := s.ApplyHostResult(ctx, update); !applied {
		t.Fatal("received -> rejected")
	}

	// Wrong host/device and expired rows are not updated.
	row4 := newRequest("r4", "h1", "d1", "c4", []byte("d"), whalereinv1.OnlineStatus_received, store.ForwardPending, now)
	_, _ = s.CreateRequest(ctx, row4)
	update = store.HostResultUpdate{RequestID: "r4", HostID: "h2", DeviceID: "d1", Status: whalereinv1.OnlineStatus_succeeded, At: now}
	if applied, _ := s.ApplyHostResult(ctx, update); applied {
		t.Fatal("wrong host applied")
	}
	expired := newRequest("r5", "h1", "d1", "c5", []byte("d"), whalereinv1.OnlineStatus_received, store.ForwardPending, now)
	expired.ExpiresAt = now.Add(-time.Minute)
	_, _ = s.CreateRequest(ctx, expired)
	update = store.HostResultUpdate{RequestID: "r5", HostID: "h1", DeviceID: "d1", Status: whalereinv1.OnlineStatus_succeeded, At: now, ResultBody: []byte("late")}
	if applied, _ := s.ApplyHostResult(ctx, update); !applied {
		t.Fatal("expired row status should still transition")
	}
	late, _ := s.GetRequest(ctx, "r5")
	if late.Status != whalereinv1.OnlineStatus_succeeded || len(late.ResultBody) != 0 {
		t.Fatalf("late result = %+v", late)
	}
	update.Status = whalereinv1.OnlineStatus_online_status_unspecified
	if _, err := s.ApplyHostResult(ctx, update); err == nil {
		t.Fatal("expected invalid status error")
	}
}

// When requests and invitations expire, cleanup clears request bodies while preserving receipts
// and removes expired pairing tokens.
func TestCleanupClearsExpiredBodiesAndDeletesExpiredTokens(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	now := time.Now().UTC()
	seedDevice(t, s, "h1", "d1")
	row := newRequest("r1", "h1", "d1", "c1", []byte("d"), whalereinv1.OnlineStatus_received, store.ForwardPending, now)
	row.ExpiresAt = now.Add(-time.Minute)
	if _, err := s.CreateRequest(ctx, row); err != nil {
		t.Fatal(err)
	}
	cleared, err := s.ClearExpiredBodies(ctx, now)
	if err != nil || cleared != 1 {
		t.Fatalf("cleared = %d err=%v", cleared, err)
	}
	got, _ := s.GetRequest(ctx, "r1")
	if len(got.RequestBody) != 0 || len(got.ResultBody) != 0 {
		t.Fatalf("bodies not cleared: %+v", got)
	}
	if got.RequestID != "r1" || got.CommandID != "c1" || !bytes.Equal(got.PayloadDigest, []byte("d")) {
		t.Fatalf("identity lost: %+v", got)
	}
	if cleared, _ := s.ClearExpiredBodies(ctx, now); cleared != 0 {
		t.Fatalf("second clear = %d", cleared)
	}
	if _, err := s.Pool().Exec(ctx, `UPDATE requests SET request_body = 'x'::bytea WHERE request_id = 'r1'`); err != nil {
		t.Fatal(err)
	}
	if cleared, _ := s.ClearExpiredBodies(ctx, now.Add(time.Hour)); cleared != 1 {
		t.Fatalf("future clear = %d", cleared)
	}
}

// When a store is opened and migrations run repeatedly, database connectivity succeeds and the
// schema remains usable.
func TestStoreOpensPingsAndAppliesMigrationsIdempotently(t *testing.T) {
	_, db := storetest.New(t)
	ctx := context.Background()
	opened, err := store.Open(ctx, db.DSN)
	if err != nil {
		t.Fatalf("Open: %v", err)
	}
	if err := opened.Ping(ctx); err != nil {
		t.Fatalf("Ping: %v", err)
	}
	seedDevice(t, opened, "migration-host", "migration-device")
	before, err := opened.DeviceByID(ctx, "migration-device")
	if err != nil {
		t.Fatal("migration fixture device missing")
	}
	if err := opened.Migrate(ctx); err != nil {
		t.Fatalf("second Migrate: %v", err)
	}
	after, err := opened.DeviceByID(ctx, "migration-device")
	if err != nil || before.PublicKeyFingerprint != after.PublicKeyFingerprint || !bytes.Equal(before.AuthHash, after.AuthHash) {
		t.Fatal("repeated migration changed a persisted device identity or credential")
	}
	if active, err := opened.HasActiveGrant(ctx, "migration-host", "migration-device"); err != nil || !active {
		t.Fatal("repeated migration lost a persisted grant")
	}
	opened.Close()
	opened.Close()
	if err := (*store.Store)(nil).Ping(ctx); err == nil {
		t.Fatal("nil store ping should fail")
	}
	if _, err := store.Open(ctx, "postgres://%zz"); err == nil {
		t.Fatal("invalid DSN should fail")
	}
	if _, err := store.Open(ctx, "postgres://postgres@127.0.0.1:1/none?sslmode=disable&connect_timeout=1"); err == nil {
		t.Fatal("unreachable database should fail")
	}
}

// When the database pool is closed, store operations return errors instead of reporting success.
func TestClosedPoolReturnsErrorsForStoreOperations(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	s.Close()
	now := time.Now().UTC()
	hash := []byte("hash")
	if _, err := s.EnsureHost(ctx, "h1"); err == nil {
		t.Fatal("EnsureHost")
	}
	if _, err := s.GetHost(ctx, "h1"); err == nil {
		t.Fatal("GetHost")
	}
	if err := s.UpdateHostMetadata(ctx, "h1", "fp", 1, now); err == nil {
		t.Fatal("UpdateHostMetadata")
	}
	if _, err := s.ListAuthorizedHosts(ctx, "d1"); err == nil {
		t.Fatal("ListAuthorizedHosts")
	}
	if _, err := s.ActiveDeviceByAuthHash(ctx, hash); err == nil {
		t.Fatal("ActiveDeviceByAuthHash")
	}
	if _, err := s.DeviceByID(ctx, "d1"); err == nil {
		t.Fatal("DeviceByID")
	}
	if _, err := s.HasActiveGrant(ctx, "h1", "d1"); err == nil {
		t.Fatal("HasActiveGrant")
	}
	if err := s.CreatePairedDevice(ctx, "h1", "d1", "fp", hash, now); err == nil {
		t.Fatal("CreatePairedDevice")
	}
	if err := s.RevokeGrant(ctx, "h1", "d1", now); err == nil {
		t.Fatal("RevokeGrant")
	}
	if _, _, err := s.RevokeDeviceGrants(ctx, "d1", now); err == nil {
		t.Fatal("RevokeDeviceGrants")
	}
	if _, err := s.PendingRevocations(ctx, "h1"); err == nil {
		t.Fatal("PendingRevocations")
	}
	if err := s.MarkRevocationsReplayed(ctx, "h1", []string{"d1"}, now, now, nil); err == nil {
		t.Fatal("MarkRevocationsReplayed")
	}
	if _, err := s.CountActiveGrants(ctx, "d1"); err == nil {
		t.Fatal("CountActiveGrants")
	}
	if err := s.CreatePairingToken(ctx, hash, "h1", now, now); err == nil {
		t.Fatal("CreatePairingToken")
	}
	if _, err := s.PairingTokenByHash(ctx, hash); err == nil {
		t.Fatal("PairingTokenByHash")
	}
	if _, err := s.IncrementPairingFailure(ctx, hash, now); err == nil {
		t.Fatal("IncrementPairingFailure")
	}
	if _, err := s.MarkPairingTokenUsed(ctx, hash, now); err == nil {
		t.Fatal("MarkPairingTokenUsed")
	}
	if _, err := s.DeleteExpiredPairingTokens(ctx, now); err == nil {
		t.Fatal("DeleteExpiredPairingTokens")
	}
	row := newRequest("r1", "h1", "d1", "c1", []byte("d"), whalereinv1.OnlineStatus_received, store.ForwardPending, now)
	if _, err := s.CreateRequest(ctx, row); err == nil {
		t.Fatal("CreateRequest")
	}
	row.CommandID = ""
	if _, err := s.CreateRequest(ctx, row); err == nil {
		t.Fatal("CreateRequest no command")
	}
	if _, err := s.GetRequest(ctx, "r1"); err == nil {
		t.Fatal("GetRequest")
	}
	if _, err := s.ClaimDueRequests(ctx, now, 1, time.Second); err == nil {
		t.Fatal("ClaimDueRequests")
	}
	if _, err := s.RequeueDuePublished(ctx, now, 1); err == nil {
		t.Fatal("RequeueDuePublished")
	}
	if err := s.MarkPublished(ctx, "r1", now, time.Second); err == nil {
		t.Fatal("MarkPublished")
	}
	if err := s.MarkPublishFailure(ctx, "r1", now, time.Second, 1); err == nil {
		t.Fatal("MarkPublishFailure")
	}
	if _, err := s.ApplyHostResult(ctx, store.HostResultUpdate{RequestID: "r1", HostID: "h1", DeviceID: "d1", Status: whalereinv1.OnlineStatus_succeeded, At: now}); err == nil {
		t.Fatal("ApplyHostResult")
	}
	if _, err := s.ClearExpiredBodies(ctx, now); err == nil {
		t.Fatal("ClearExpiredBodies")
	}
	if _, err := s.CountRequests(ctx); err == nil {
		t.Fatal("CountRequests")
	}
	if err := s.Migrate(ctx); err == nil {
		t.Fatal("Migrate")
	}
}

// When Access reopens the persisted schema, active grants and pending requests survive and
// forwarding can resume.
func TestAccessRestartKeepsGrantsAndPendingRequests(t *testing.T) {
	first, db := storetest.New(t)
	ctx := context.Background()
	now := time.Now().UTC()
	seedDevice(t, first, "h1", "d1")
	row := newRequest("r1", "h1", "d1", "c1", []byte("d"), whalereinv1.OnlineStatus_received, store.ForwardPending, now)
	if _, err := first.CreateRequest(ctx, row); err != nil {
		t.Fatal(err)
	}
	first.Close()

	second := db.Open(t)
	if active, err := second.HasActiveGrant(ctx, "h1", "d1"); err != nil || !active {
		t.Fatalf("grant after restart = %v err=%v", active, err)
	}
	restored, err := second.GetRequest(ctx, "r1")
	if err != nil || restored.RequestID != "r1" || restored.ForwardState != store.ForwardPending {
		t.Fatalf("request after restart = %+v err=%v", restored, err)
	}
	claimed, err := second.ClaimDueRequests(ctx, now, 10, time.Minute)
	if err != nil || len(claimed) != 1 || claimed[0].RequestID != "r1" {
		t.Fatalf("claim after restart = %+v err=%v", claimed, err)
	}
}

// fixturePairingOperation creates the fenced HELLO and a matching opaque FINISH.
func fixturePairingOperation(t *testing.T, s *store.Store, hash []byte, hostID, deviceID, claimID string, at time.Time, notice *whalereinv1.HostRequest) (store.PairingOperation, *whalereinv1.HostRequest) {
	t.Helper()
	ctx := context.Background()
	deviceGeneration, err := s.DeviceGeneration(ctx, deviceID)
	if err != nil {
		t.Fatal(err)
	}
	hostGeneration, err := s.HostGeneration(ctx, hostID, deviceID)
	if err != nil {
		t.Fatal(err)
	}
	op := store.PairingOperation{ClaimID: claimID, TokenHash: hash, HostID: hostID, DeviceID: deviceID, HelloHash: bytes.Repeat([]byte{1}, 32), HelloResponse: []byte("fixture"), ExpiresAt: at.Add(time.Minute), DeviceGeneration: deviceGeneration, HostGeneration: hostGeneration, FinishHash: bytes.Repeat([]byte{2}, 32)}
	if err := s.SavePairingOperation(ctx, op); err != nil {
		t.Fatal(err)
	}
	pub := bytes.Repeat([]byte{3}, 32)
	op.FinishResponse, _ = proto.Marshal(&whalereinv1.PairingClaimResponse{ClaimId: claimID, HostId: hostID, DeviceId: deviceID, Phase: whalereinv1.PairingPhase_PAIRING_PHASE_FINISH, DevicePublicKey: pub, ProtocolVersion: 1})
	if notice == nil {
		notice = &whalereinv1.HostRequest{RequestId: "fixture-" + claimID, HostId: hostID, DeviceId: deviceID, Kind: whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE, ProtocolVersion: 1, ExpiresAtUnixMs: op.ExpiresAt.UnixMilli()}
	}
	if notice.ProtocolVersion == 0 {
		notice.ProtocolVersion = 1
	}
	notice.Body, _ = proto.Marshal(&whalereinv1.PairingClaim{ClaimId: claimID, DeviceId: deviceID, DevicePublicKey: pub, Phase: whalereinv1.PairingPhase_PAIRING_PHASE_COMMIT, ExpiresAtUnixMs: notice.ExpiresAtUnixMs})
	return op, notice
}

func fixtureKeyPin(value byte) string {
	key := bytes.Repeat([]byte{value}, 32)
	hash := sha256.Sum256(key)
	return hex.EncodeToString(hash[:])
}

// When pairing conflicts with a pinned identity, the transaction preserves the invitation and old
// credential; valid pairing commits once.
func TestPairingCommitKeepsGrantAtomicAndHostIdentityPinned(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	now := time.Now().UTC()
	if _, err := s.EnsureHost(ctx, "h1"); err != nil {
		t.Fatal(err)
	}
	if err := s.CreatePairedDevice(ctx, "h1", "d1", fixtureKeyPin(3), []byte("old-credential"), now); err != nil {
		t.Fatal(err)
	}
	hash := bytes.Repeat([]byte{7}, 32)
	if err := s.CreatePairingToken(ctx, hash, "h1", now.Add(time.Minute), now); err != nil {
		t.Fatal(err)
	}
	if _, err := s.CommitPairing(ctx, hash, "h1", "d1", fixtureKeyPin(3), []byte("unfenced"), now, nil, nil); !errors.Is(err, store.ErrConflict) {
		t.Fatal("unfenced pairing accepted")
	}
	op, notice := fixturePairingOperation(t, s, hash, "h1", "d1", "atomic", now, nil)
	differentOp := op
	var differentResponse whalereinv1.PairingClaimResponse
	if err := proto.Unmarshal(op.FinishResponse, &differentResponse); err != nil {
		t.Fatal(err)
	}
	differentResponse.DevicePublicKey = bytes.Repeat([]byte{4}, 32)
	differentOp.FinishResponse, _ = proto.Marshal(&differentResponse)
	differentNotice := proto.Clone(notice).(*whalereinv1.HostRequest)
	var differentClaim whalereinv1.PairingClaim
	if err := proto.Unmarshal(differentNotice.Body, &differentClaim); err != nil {
		t.Fatal(err)
	}
	differentClaim.DevicePublicKey = bytes.Repeat([]byte{4}, 32)
	differentNotice.Body, _ = proto.Marshal(&differentClaim)
	if _, err := s.CommitPairing(ctx, hash, "h1", "d1", fixtureKeyPin(4), []byte("new-credential"), now, differentNotice, &differentOp); !errors.Is(err, store.ErrConflict) {
		t.Fatal("identity was rebound")
	}
	token, err := s.PairingTokenByHash(ctx, hash)
	if err != nil || token.UsedAt != nil {
		t.Fatal("failed commit consumed invitation")
	}
	if _, err := s.ActiveDeviceByAuthHash(ctx, []byte("old-credential")); err != nil {
		t.Fatal("failed commit rotated existing credential")
	}
	if used, err := s.CommitPairing(ctx, hash, "h1", "d1", fixtureKeyPin(3), []byte("new-credential"), now, notice, &op); err != nil || !used {
		t.Fatal("valid pairing failed")
	}
	if used, err := s.CommitPairing(ctx, hash, "h1", "d1", fixtureKeyPin(3), []byte("another"), now, notice, &op); err != nil || used {
		t.Fatal("invitation reused")
	}
	if _, err := s.ActiveDeviceByAuthHash(ctx, []byte("new-credential")); err != nil {
		t.Fatal("committed credential missing")
	}
	s.Close()
	if _, err := s.CommitPairing(ctx, hash, "h1", "d1", fixtureKeyPin(3), nil, now, notice, &op); err == nil {
		t.Fatal("closed database accepted commit")
	}
}

// When pairing commits an activation notice, the grant and outbox row persist together; notice
// conflicts roll back invitation and credential changes.
func TestPairingCommitPersistsActivationNoticeAndRollsBackConflicts(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	at := time.Now().UTC()
	if _, err := s.EnsureHost(ctx, "h"); err != nil {
		t.Fatal(err)
	}
	hash := bytes.Repeat([]byte{9}, 32)
	if err := s.CreatePairingToken(ctx, hash, "h", at.Add(time.Minute), at); err != nil {
		t.Fatal(err)
	}
	notice := &whalereinv1.HostRequest{RequestId: "activate", HostId: "other", DeviceId: "d", Body: []byte{}, Kind: whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE, ProtocolVersion: 1, ExpiresAtUnixMs: at.Add(time.Minute).UnixMilli()}
	op, notice := fixturePairingOperation(t, s, hash, "h", "d", "activation", at, notice)
	if _, err := s.CommitPairing(ctx, hash, "h", "d", fixtureKeyPin(3), []byte("credential"), at, notice, &op); !errors.Is(err, store.ErrConflict) {
		t.Fatal("invalid activation route accepted")
	}
	notice.HostId = "h"
	if used, err := s.CommitPairing(ctx, hash, "h", "d", fixtureKeyPin(3), []byte("credential"), at, notice, &op); err != nil || !used {
		t.Fatal("activation notice not committed")
	}
	row, err := s.GetRequest(ctx, "activate")
	if err != nil || row.ForwardState != store.ForwardPending || row.Kind != notice.Kind {
		t.Fatal("activation outbox missing")
	}
	var activation whalereinv1.PairingClaim
	if err := proto.Unmarshal(row.RequestBody, &activation); err != nil {
		t.Fatal(err)
	}
	if activation.GrantGeneration != 1 || !bytes.Equal(row.RequestBody, notice.Body) {
		t.Fatal("direct and durable COMMIT incarnations differ")
	}
	next := bytes.Repeat([]byte{10}, 32)
	if err := s.CreatePairingToken(ctx, next, "h", at.Add(time.Minute), at); err != nil {
		t.Fatal(err)
	}
	nextOp, nextNotice := fixturePairingOperation(t, s, next, "h", "d", "activation-next", at, notice)
	if _, err := s.CommitPairing(ctx, next, "h", "d", fixtureKeyPin(3), []byte("replacement"), at, nextNotice, &nextOp); err == nil {
		t.Fatal("duplicate activation record ignored")
	}
	token, err := s.PairingTokenByHash(ctx, next)
	if err != nil || token.UsedAt != nil {
		t.Fatal("outbox failure consumed invitation")
	}
	if _, err := s.ActiveDeviceByAuthHash(ctx, []byte("credential")); err != nil {
		t.Fatal("outbox failure rotated credential")
	}
}

// When activation delivery exceeds its ordinary retry budget, it continues retrying until its
// deadline and is not forwarded after expiry.
func TestActivationNoticeRetriesPastDeliveryBudgetUntilItsDeadline(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	at := time.Now().UTC()
	if _, err := s.EnsureHost(ctx, "h"); err != nil {
		t.Fatal(err)
	}
	hash := bytes.Repeat([]byte{12}, 32)
	if err := s.CreatePairingToken(ctx, hash, "h", at.Add(time.Minute), at); err != nil {
		t.Fatal(err)
	}
	notice := &whalereinv1.HostRequest{RequestId: "retry-activation", HostId: "h", DeviceId: "d", Kind: whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE, Body: []byte{}, ExpiresAtUnixMs: at.Add(time.Minute).UnixMilli()}
	op, notice := fixturePairingOperation(t, s, hash, "h", "d", "activation-retry", at, notice)
	if used, err := s.CommitPairing(ctx, hash, "h", "d", fixtureKeyPin(3), []byte("credential"), at, notice, &op); err != nil || !used {
		t.Fatal("commit failed")
	}
	for index := 0; index < 6; index++ {
		rows, err := s.ClaimDueRequests(ctx, at, 1, time.Millisecond)
		if err != nil || len(rows) != 1 {
			t.Fatal("activation stopped retrying")
		}
		if err := s.MarkPublishFailure(ctx, notice.RequestId, at, 0, 1); err != nil {
			t.Fatal(err)
		}
	}
	if err := s.MarkPublished(ctx, notice.RequestId, at, 0); err != nil {
		t.Fatal(err)
	}
	if count, err := s.RequeueDuePublished(ctx, at, 1); err != nil || count != 1 {
		t.Fatal("published activation was not requeued")
	}
	if rows, err := s.ClaimDueRequests(ctx, at.Add(2*time.Minute), 1, time.Second); err != nil || len(rows) != 0 {
		t.Fatal("expired activation was forwarded")
	}
}

// When a request is stored and loaded, receiver transport capability is preserved and unsupported
// values are rejected.
func TestReceiverTransportCapabilitySurvivesDatabaseRoundtrip(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	at := time.Now().UTC()
	if _, err := s.EnsureHost(ctx, "h"); err != nil {
		t.Fatal(err)
	}
	if err := s.CreatePairedDevice(ctx, "h", "d", "fp", []byte("fixture-credential"), at); err != nil {
		t.Fatal(err)
	}
	request := newRequest("capability", "h", "d", "", nil, whalereinv1.OnlineStatus_received, store.ForwardPending, at)
	request.ClientTransportVersion = 2
	if _, err := s.CreateRequest(ctx, request); err != nil {
		t.Fatal(err)
	}
	loaded, err := s.GetRequest(ctx, request.RequestID)
	if err != nil || loaded.ClientTransportVersion != 2 {
		t.Fatal("capability not persisted")
	}
	request.ClientTransportVersion = ^uint32(0)
	request.RequestID = "invalid-capability"
	if _, err := s.CreateRequest(ctx, request); err == nil {
		t.Fatal("invalid capability accepted")
	}
}

// When a device repairs its grant, old revocation ACKs cannot consume the next incarnation.
func TestGrantGenerationSurvivesRepairAndFencesRevocationAcknowledgements(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	at := time.Now().UTC()
	if _, err := s.EnsureHost(ctx, "h"); err != nil {
		t.Fatal(err)
	}
	for generation := uint64(1); generation <= 2; generation++ {
		if err := s.CreatePairedDevice(ctx, "h", "d", "fp", []byte("fixture"), at); err != nil {
			t.Fatal(err)
		}
		if err := s.RevokeGrant(ctx, "h", "d", at); err != nil {
			t.Fatal(err)
		}
		pending, err := s.PendingRevocations(ctx, "h")
		if err != nil || len(pending) != 1 || pending[0].GrantGeneration != generation {
			t.Fatal("grant incarnation did not advance")
		}
	}
	if err := s.MarkRevocationsReplayed(ctx, "h", []string{"d"}, at.Add(time.Second), at, map[string]uint64{"d": 1}); err != nil {
		t.Fatal(err)
	}
	pending, err := s.PendingRevocations(ctx, "h")
	if err != nil || len(pending) != 1 {
		t.Fatal("old ACK consumed a newer revocation")
	}
	if err := s.MarkRevocationsReplayed(ctx, "h", []string{"d"}, at.Add(time.Second), at, map[string]uint64{"d": 2}); err != nil {
		t.Fatal(err)
	}
	pending, err = s.PendingRevocations(ctx, "h")
	if err != nil || len(pending) != 0 {
		t.Fatal("current ACK was not applied")
	}
}

// Versioned migrations preserve existing state, reject edited history, and refuse a newer database.
func TestMigrationLedgerRejectsChangedHistoryAndFutureSchema(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	if err := s.Migrate(ctx); err != nil {
		t.Fatal(err)
	}
	entries, err := embeddedMigrations.ReadDir("migrations")
	if err != nil {
		t.Fatal(err)
	}
	var expected int
	for _, entry := range entries {
		if !entry.IsDir() && strings.HasSuffix(entry.Name(), ".sql") {
			expected++
			var present bool
			if err := s.Pool().QueryRow(ctx, "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE name=$1)", entry.Name()).Scan(&present); err != nil || !present {
				t.Fatalf("migration %s missing: %v", entry.Name(), err)
			}
		}
	}
	var count int
	if err := s.Pool().QueryRow(ctx, "SELECT count(*) FROM schema_migrations").Scan(&count); err != nil || count != expected {
		t.Fatalf("migration versions: got %d, expected %d", count, expected)
	}
	if _, err := s.Pool().Exec(ctx, "UPDATE schema_migrations SET checksum='modified'::bytea WHERE name='005_standalone.sql'"); err != nil {
		t.Fatal(err)
	}
	if err := s.Migrate(ctx); err == nil || !strings.Contains(err.Error(), "checksum changed") {
		t.Fatal("edited migration accepted")
	}
	if _, err := s.Pool().Exec(ctx, "INSERT INTO schema_migrations(name,checksum) VALUES('999_future.sql','future'::bytea)"); err != nil {
		t.Fatal(err)
	}
	if err := s.Migrate(ctx); err == nil || !strings.Contains(err.Error(), "newer migration") {
		t.Fatal("older binary accepted newer schema")
	}
}

// Independent Host pairing preserves old credentials, and the last revocation permanently invalidates them.
func TestCredentialsSurviveAnotherHostButNotGlobalRevocation(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	now := time.Now().UTC()
	for _, host := range []string{"a", "b"} {
		if _, err := s.EnsureHost(ctx, host); err != nil {
			t.Fatal(err)
		}
	}
	old := bytes.Repeat([]byte{1}, 32)
	next := bytes.Repeat([]byte{2}, 32)
	if err := s.CreatePairedDevice(ctx, "a", "d", "pinned", old, now); err != nil {
		t.Fatal(err)
	}
	if err := s.CreatePairedDevice(ctx, "b", "d", "pinned", next, now); err != nil {
		t.Fatal(err)
	}
	if _, err := s.ActiveDeviceByAuthHash(ctx, old); err != nil {
		t.Fatal("another Host invalidated an existing credential")
	}
	if err := s.RevokeGrant(ctx, "a", "d", now); err != nil {
		t.Fatal(err)
	}
	if _, err := s.ActiveDeviceByAuthHash(ctx, old); err != nil {
		t.Fatal("scoped revoke invalidated another Host")
	}
	if _, _, err := s.RevokeDeviceGrants(ctx, "d", now); err != nil {
		t.Fatal(err)
	}
	if err := s.CreatePairedDevice(ctx, "b", "d", "pinned", bytes.Repeat([]byte{3}, 32), now); err != nil {
		t.Fatal(err)
	}
	for _, hash := range [][]byte{old, next} {
		if _, err := s.ActiveDeviceByAuthHash(ctx, hash); !errors.Is(err, store.ErrNotFound) {
			t.Fatal("old credential revived")
		}
	}
	var redundant int
	if err := s.Pool().QueryRow(ctx, `SELECT count(*) FROM information_schema.columns
        WHERE table_schema=current_schema() AND table_name IN ('hosts','devices','grants')
        AND column_name IN ('workspace_id','auth_hash')`).Scan(&redundant); err != nil || redundant != 0 {
		t.Fatal("fixed scope or duplicate credential state remains")
	}
}

//go:embed migrations/001_init.sql migrations/002_host_authorization_key.sql migrations/003_client_transport.sql migrations/004_grant_generation.sql
var legacyMigrations embed.FS

// When a populated unversioned installation is upgraded, migration adoption preserves its identities, credential and active grant.
func TestMigrationAdoptsPopulatedUnversionedInstallation(t *testing.T) {
	db := storetest.NewSchema(t)
	ctx := context.Background()
	pool, err := pgxpool.New(ctx, db.DSN)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(pool.Close)
	entries, err := legacyMigrations.ReadDir("migrations")
	if err != nil {
		t.Fatal(err)
	}
	for _, entry := range entries {
		body, err := legacyMigrations.ReadFile("migrations/" + entry.Name())
		if err != nil {
			t.Fatal(err)
		}
		if _, err := pool.Exec(ctx, string(body)); err != nil {
			t.Fatal(err)
		}
	}
	if _, err := pool.Exec(ctx, `INSERT INTO hosts(host_id,fingerprint) VALUES('legacy-host','pinned');
        INSERT INTO devices(device_id,public_key_fingerprint,auth_hash) VALUES('legacy-device','device-pin','legacy-fixture-hash'::bytea);
        INSERT INTO grants(host_id,device_id) VALUES('legacy-host','legacy-device');`); err != nil {
		t.Fatal(err)
	}
	s := store.NewWithPool(pool)
	if err := s.Migrate(ctx); err != nil {
		t.Fatal(err)
	}
	host, err := s.GetHost(ctx, "legacy-host")
	if err != nil || host.Fingerprint != "pinned" {
		t.Fatal("Host identity changed during adoption")
	}
	device, err := s.ActiveDeviceByAuthHash(ctx, []byte("legacy-fixture-hash"))
	if err != nil || device.DeviceID != "legacy-device" || device.PublicKeyFingerprint != "device-pin" {
		t.Fatal("device credential changed during adoption")
	}
	if active, err := s.HasActiveGrant(ctx, "legacy-host", "legacy-device"); err != nil || !active {
		t.Fatal("grant lost during adoption")
	}
	if err := s.Migrate(ctx); err != nil {
		t.Fatal(err)
	}
}

// When a pairing grant is replaced while its credential remains valid for another Host, the old completion receipt cannot recover the replacement grant or persist plaintext credentials.
func TestPairingReceiptRequiresExactActiveGrantGeneration(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	now := time.Now().UTC()
	for _, host := range []string{"receipt-host", "other-host"} {
		if _, err := s.EnsureHost(ctx, host); err != nil {
			t.Fatal(err)
		}
	}
	invitationHash := bytes.Repeat([]byte{1}, 32)
	if err := s.CreatePairingToken(ctx, invitationHash, "receipt-host", now.Add(time.Hour), now); err != nil {
		t.Fatal(err)
	}
	op := store.PairingOperation{ClaimID: "receipt", HostID: "receipt-host", DeviceID: "receipt-device", TokenHash: invitationHash,
		HelloHash: bytes.Repeat([]byte{2}, 32), HelloResponse: []byte("opaque fixture reply"), ExpiresAt: now.Add(time.Hour), FinishHash: bytes.Repeat([]byte{3}, 32)}
	if err := s.SavePairingOperation(ctx, op); err != nil {
		t.Fatal(err)
	}
	pub := bytes.Repeat([]byte{4}, 32)
	digest := sha256.Sum256(pub)
	devicePin := hex.EncodeToString(digest[:])
	response := &whalereinv1.PairingClaimResponse{ClaimId: op.ClaimID, HostId: op.HostID, DeviceId: op.DeviceID,
		Phase: whalereinv1.PairingPhase_PAIRING_PHASE_FINISH, DevicePublicKey: pub, ProtocolVersion: 1, DeviceToken: "must-not-persist-fixture"}
	claimBody, _ := proto.Marshal(&whalereinv1.PairingClaim{ClaimId: op.ClaimID, DeviceId: op.DeviceID,
		DevicePublicKey: pub, Phase: whalereinv1.PairingPhase_PAIRING_PHASE_COMMIT, ExpiresAtUnixMs: op.ExpiresAt.UnixMilli()})
	notice := &whalereinv1.HostRequest{RequestId: "receipt-commit", HostId: op.HostID, DeviceId: op.DeviceID,
		Kind: whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE, Body: claimBody, ProtocolVersion: 1, ExpiresAtUnixMs: op.ExpiresAt.UnixMilli()}
	authHash := bytes.Repeat([]byte{5}, 32)
	op.FinishResponse, _ = proto.Marshal(response)
	if _, err := s.CommitPairing(ctx, invitationHash, op.HostID, op.DeviceID, devicePin, authHash, now, notice, &op); !errors.Is(err, store.ErrConflict) {
		t.Fatal("plaintext credential receipt accepted")
	}
	response.DeviceToken = ""
	op.FinishResponse, _ = proto.Marshal(response)
	if used, err := s.CommitPairing(ctx, invitationHash, op.HostID, op.DeviceID, devicePin, authHash, now, notice, &op); err != nil || !used {
		t.Fatal("valid pairing commit failed")
	}
	saved, err := s.PairingOperation(ctx, op.ClaimID)
	if err != nil || !saved.Active {
		t.Fatal("committed receipt not recoverable")
	}
	outcome, err := s.PairingOutcome(ctx, op.ClaimID, op.DeviceID)
	if err != nil || !outcome.Active || !bytes.Equal(outcome.TokenHash, invitationHash) || !bytes.Equal(outcome.FinishHash, op.FinishHash) {
		t.Fatal("committed outcome mismatch")
	}
	if _, err := s.ConfirmPairingHost(ctx, op.ClaimID, op.HostID, op.DeviceID, "wrong-request", now); err == nil {
		t.Fatal("wrong request confirmed Host")
	}
	if confirmed, err := s.ConfirmPairingHost(ctx, op.ClaimID, op.HostID, op.DeviceID, notice.RequestId, now); err != nil || !confirmed {
		t.Fatal("valid Host confirmation not saved")
	}
	outcome, err = s.PairingOutcome(ctx, op.ClaimID, op.DeviceID)
	if err != nil || !outcome.HostConfirmed || !bytes.Equal(outcome.DevicePublicKey, pub) || outcome.ProtocolVersion != 1 {
		t.Fatal("Host confirmation binding lost")
	}

	if err := s.CreatePairedDevice(ctx, "other-host", op.DeviceID, devicePin, bytes.Repeat([]byte{6}, 32), now); err != nil {
		t.Fatal(err)
	}
	if err := s.CreatePairedDevice(ctx, op.HostID, op.DeviceID, devicePin, bytes.Repeat([]byte{7}, 32), now); err != nil {
		t.Fatal(err)
	}
	if _, err := s.ActiveDeviceByAuthHash(ctx, authHash); err != nil {
		t.Fatal("other Host credential lost")
	}
	saved, err = s.PairingOperation(ctx, op.ClaimID)
	if err != nil || saved.Active {
		t.Fatal("superseded grant recovered through old receipt")
	}
	if confirmed, err := s.ConfirmPairingHost(ctx, op.ClaimID, op.HostID, op.DeviceID, notice.RequestId, now); err != nil || confirmed {
		t.Fatal("superseded grant was confirmed")
	}

	if _, err := s.DeleteExpiredPairingTokens(ctx, now.Add(2*time.Hour)); err != nil {
		t.Fatal(err)
	}
	outcome, err = s.PairingOutcome(ctx, op.ClaimID, op.DeviceID)
	if err != nil || outcome.Active || !bytes.Equal(outcome.TokenHash, invitationHash) || !bytes.Equal(outcome.FinishHash, op.FinishHash) {
		t.Fatal("cleanup lost or changed committed evidence")
	}
	if _, err := s.PairingOutcome(ctx, op.ClaimID, "wrong-device"); !errors.Is(err, store.ErrNotFound) {
		t.Fatal("outcome disclosed to a different device")
	}
}

// When read-only requests outlive their retention window, cleanup removes only those records and preserves control/activation uncertainty.
func TestQueryRetentionPreservesAllControlAndActivationIdentities(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	now := time.Now().UTC()
	seedDevice(t, s, "host", "device")
	for _, kind := range []whalereinv1.RequestKind{whalereinv1.RequestKind_REQUEST_KIND_HANDSHAKE, whalereinv1.RequestKind_REQUEST_KIND_QUERY, whalereinv1.RequestKind_REQUEST_KIND_COMMAND, whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE} {
		id := fmt.Sprintf("kind-%d", kind)
		commandID := ""
		if kind == whalereinv1.RequestKind_REQUEST_KIND_COMMAND {
			commandID = "control"
		}
		request := newRequest(id, "host", "device", commandID, []byte("fixture-digest"), whalereinv1.OnlineStatus_unknown, store.ForwardNone, now.Add(-72*time.Hour))
		request.Kind = kind
		if _, err := s.CreateRequest(ctx, request); err != nil {
			t.Fatal(err)
		}
	}
	recent := newRequest("recent", "host", "device", "", nil, whalereinv1.OnlineStatus_succeeded, store.ForwardNone, now)
	recent.Kind = whalereinv1.RequestKind_REQUEST_KIND_QUERY
	if _, err := s.CreateRequest(ctx, recent); err != nil {
		t.Fatal(err)
	}
	if deleted, err := s.DeleteExpiredQueries(ctx, now.Add(-24*time.Hour)); err != nil || deleted != 2 {
		t.Fatal("query retention did not isolate read-only records")
	}
	for _, id := range []string{"kind-3", "kind-5", "recent"} {
		if _, err := s.GetRequest(ctx, id); err != nil {
			t.Fatal("retention deleted protected identity")
		}
	}
	if deleted, err := s.DeleteExpiredQueries(ctx, now.Add(-24*time.Hour)); err != nil || deleted != 0 {
		t.Fatal("query retention not idempotent")
	}
}

// A global revocation fences HELLOs already in flight but permits a later explicit pairing.
func TestPairingRevocationFenceOrdersAndPreservesFreshPairing(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	now := time.Now().UTC()
	for _, id := range []string{"fence-primary", "fence-secondary"} {
		if _, err := s.EnsureHost(ctx, id); err != nil {
			t.Fatal(err)
		}
	}
	if err := s.CreatePairedDevice(ctx, "fence-primary", "fence-device", fixtureKeyPin(4), bytes.Repeat([]byte{9}, 32), now); err != nil {
		t.Fatal(err)
	}
	makeOperation := func(id string, value byte, generation int64) (store.PairingOperation, *whalereinv1.HostRequest, []byte) {
		hash := bytes.Repeat([]byte{value}, 32)
		if err := s.CreatePairingToken(ctx, hash, "fence-secondary", now.Add(time.Hour), now); err != nil {
			t.Fatal(err)
		}
		op := store.PairingOperation{ClaimID: id, TokenHash: hash, HostID: "fence-secondary", DeviceID: "fence-device", DeviceGeneration: generation,
			HelloHash: bytes.Repeat([]byte{2}, 32), HelloResponse: []byte("fixture"), ExpiresAt: now.Add(time.Hour), FinishHash: bytes.Repeat([]byte{3}, 32)}
		pub := bytes.Repeat([]byte{4}, 32)
		response := &whalereinv1.PairingClaimResponse{ClaimId: id, HostId: op.HostID, DeviceId: op.DeviceID, Phase: whalereinv1.PairingPhase_PAIRING_PHASE_FINISH, DevicePublicKey: pub, ProtocolVersion: 1}
		op.FinishResponse, _ = proto.Marshal(response)
		body, _ := proto.Marshal(&whalereinv1.PairingClaim{ClaimId: id, DeviceId: op.DeviceID, DevicePublicKey: pub, Phase: whalereinv1.PairingPhase_PAIRING_PHASE_COMMIT, ExpiresAtUnixMs: op.ExpiresAt.UnixMilli()})
		notice := &whalereinv1.HostRequest{RequestId: "commit-" + id, HostId: op.HostID, DeviceId: op.DeviceID, Kind: whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE, Body: body, ProtocolVersion: 1, ExpiresAtUnixMs: op.ExpiresAt.UnixMilli()}
		return op, notice, hash
	}
	generation, err := s.DeviceGeneration(ctx, "fence-device")
	if err != nil || generation != 0 {
		t.Fatal("initial fence")
	}
	stale, notice, hash := makeOperation("stale", 1, generation)
	if err := s.SavePairingOperation(ctx, stale); err != nil {
		t.Fatal(err)
	}
	if _, exists, err := s.RevokeDeviceGrants(ctx, "fence-device", now); err != nil || !exists {
		t.Fatal("revoke failed")
	}
	if fenced, err := s.PairingFenced(ctx, stale.ClaimID, stale.HostID, stale.DeviceID); err != nil || !fenced {
		t.Fatal("unfinished pre-revoke claim not proven fenced")
	}
	if _, err := s.CommitPairing(ctx, hash, stale.HostID, stale.DeviceID, fixtureKeyPin(4), bytes.Repeat([]byte{5}, 32), now, notice, &stale); !errors.Is(err, store.ErrPairingFenced) {
		t.Fatal("old finish was not fenced")
	}
	if active, err := s.HasActiveGrant(ctx, stale.HostID, stale.DeviceID); err != nil || active {
		t.Fatal("stale grant became active")
	}
	late, _, _ := makeOperation("stale-hello", 2, generation)
	if err := s.SavePairingOperation(ctx, late); !errors.Is(err, store.ErrPairingFenced) {
		t.Fatal("late HELLO was not fenced")
	}
	generation, err = s.DeviceGeneration(ctx, "fence-device")
	if err != nil || generation != 1 {
		t.Fatal("fence did not advance")
	}
	fresh, freshNotice, freshHash := makeOperation("fresh", 3, generation)
	if err := s.SavePairingOperation(ctx, fresh); err != nil {
		t.Fatal(err)
	}
	if used, err := s.CommitPairing(ctx, freshHash, fresh.HostID, fresh.DeviceID, fixtureKeyPin(4), bytes.Repeat([]byte{6}, 32), now, freshNotice, &fresh); err != nil || !used {
		t.Fatal("fresh pairing rejected")
	}
	if _, exists, err := s.RevokeDeviceGrants(ctx, fresh.DeviceID, now); err != nil || !exists {
		t.Fatal("second revoke failed")
	}
	if active, err := s.HasActiveGrant(ctx, fresh.HostID, fresh.DeviceID); err != nil || active {
		t.Fatal("committed-before-revoke grant remained active")
	}
	if fenced, err := s.PairingFenced(ctx, fresh.ClaimID, fresh.HostID, fresh.DeviceID); err != nil || fenced {
		t.Fatal("committed claim was incorrectly declared discardable")
	}
	if used, err := s.CommitPairing(ctx, freshHash, fresh.HostID, fresh.DeviceID, fixtureKeyPin(4), bytes.Repeat([]byte{6}, 32), now, freshNotice, &fresh); err != nil || used {
		t.Fatal("completed claim was falsely classified as fenced")
	}

}

// A failed global revoke rolls back its fence and leaves the existing credential and grant intact.
func TestPairingRevocationFenceRollsBackWithFailedRevoke(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	now := time.Now().UTC()
	if _, err := s.EnsureHost(ctx, "fence-host"); err != nil {
		t.Fatal(err)
	}
	auth := []byte("fence-auth")
	if err := s.CreatePairedDevice(ctx, "fence-host", "fence-device", "pin", auth, now); err != nil {
		t.Fatal(err)
	}
	_, err := s.Pool().Exec(ctx, `CREATE FUNCTION fail_fence_revoke() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'fixture rollback'; END $$;
       CREATE TRIGGER fail_fence_revoke BEFORE UPDATE ON grants FOR EACH ROW EXECUTE FUNCTION fail_fence_revoke();`)
	if err != nil {
		t.Fatal(err)
	}
	if _, _, err := s.RevokeDeviceGrants(ctx, "fence-device", now); err == nil {
		t.Fatal("fixture did not fail revoke")
	}
	generation, err := s.DeviceGeneration(ctx, "fence-device")
	if err != nil || generation != 0 {
		t.Fatal("failed transaction advanced fence")
	}
	if active, err := s.HasActiveGrant(ctx, "fence-host", "fence-device"); err != nil || !active {
		t.Fatal("failed transaction revoked grant")
	}
	if _, err := s.ActiveDeviceByAuthHash(ctx, auth); err != nil {
		t.Fatal("failed transaction removed credential")
	}
}

// Migration fences pre-existing unfinished claims while preserving the active grant.
func TestPairingRevocationFenceMigrationFencesLegacyPendingClaims(t *testing.T) {
	// Start from the real pre-008 schema so the upgrade runs the production
	// migration path instead of dropping columns from a current database.
	s := openStoreAtMigration(t, "017_device_sessions.sql")
	ctx := context.Background()
	now := time.Now().UTC()
	if _, err := s.EnsureHost(ctx, "migration-host"); err != nil {
		t.Fatal(err)
	}
	seedLegacyDevice(t, s, "migration-host", "migration-device")
	hash := bytes.Repeat([]byte{8}, 32)
	if err := s.CreatePairingToken(ctx, hash, "migration-host", now.Add(time.Hour), now); err != nil {
		t.Fatal(err)
	}
	_, err := s.Pool().Exec(ctx, `DROP VIEW IF EXISTS grant_authority;
      ALTER TABLE pairing_operations DROP COLUMN device_generation;
      ALTER TABLE pairing_operations DROP COLUMN host_generation;
      ALTER TABLE grants DROP COLUMN revocation_generation;
      ALTER TABLE devices DROP COLUMN revocation_generation;
      DELETE FROM schema_migrations WHERE name IN ('008_device_revocation_fence.sql','016_external_device_revocations.sql')`)
	if err != nil {
		t.Fatal(err)
	}
	_, err = s.Pool().Exec(ctx, `INSERT INTO pairing_operations(claim_id,token_hash,host_id,device_id,hello_hash,hello_response,expires_at)
      VALUES('old-pending',$1,'migration-host','migration-device',$2,$3,$4)`, hash, []byte("hash"), []byte("reply"), now.Add(time.Hour))
	if err != nil {
		t.Fatal(err)
	}
	if err := s.Migrate(ctx); err != nil {
		t.Fatal(err)
	}
	var pending int64
	if err := s.Pool().QueryRow(ctx, `SELECT device_generation FROM pairing_operations WHERE claim_id='old-pending'`).Scan(&pending); err != nil || pending != -1 {
		t.Fatal("legacy pending claim not fenced")
	}
	generation, err := s.DeviceGeneration(ctx, "migration-device")
	if err != nil || generation != 0 {
		t.Fatal("legacy device generation invalid")
	}
	if active, err := s.HasActiveGrant(ctx, "migration-host", "migration-device"); err != nil || !active {
		t.Fatal("legacy grant changed")
	}
	if err := s.Migrate(ctx); err != nil {
		t.Fatal("migration not idempotent")
	}
}

// Host-scoped revocation invalidates only that Host's prior pairing intent.
func TestPairingRevocationFenceIsScopedToHost(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	now := time.Now().UTC()
	for _, host := range []string{"host-a", "host-b"} {
		if _, err := s.EnsureHost(ctx, host); err != nil {
			t.Fatal(err)
		}
		if err := s.CreatePairedDevice(ctx, host, "device", fixtureKeyPin(4), []byte("shared-auth"), now); err != nil {
			t.Fatal(err)
		}
	}
	hash := bytes.Repeat([]byte{6}, 32)
	if err := s.CreatePairingToken(ctx, hash, "host-b", now.Add(time.Hour), now); err != nil {
		t.Fatal(err)
	}
	op := store.PairingOperation{ClaimID: "host-b-old", TokenHash: hash, HostID: "host-b", DeviceID: "device", HelloHash: bytes.Repeat([]byte{2}, 32), HelloResponse: []byte("fixture"), ExpiresAt: now.Add(time.Hour), FinishHash: bytes.Repeat([]byte{3}, 32)}
	if err := s.SavePairingOperation(ctx, op); err != nil {
		t.Fatal(err)
	}
	pub := bytes.Repeat([]byte{4}, 32)
	op.FinishResponse, _ = proto.Marshal(&whalereinv1.PairingClaimResponse{ClaimId: op.ClaimID, HostId: op.HostID, DeviceId: op.DeviceID, Phase: whalereinv1.PairingPhase_PAIRING_PHASE_FINISH, DevicePublicKey: pub, ProtocolVersion: 1})
	body, _ := proto.Marshal(&whalereinv1.PairingClaim{ClaimId: op.ClaimID, DeviceId: op.DeviceID, DevicePublicKey: pub, Phase: whalereinv1.PairingPhase_PAIRING_PHASE_COMMIT, ExpiresAtUnixMs: op.ExpiresAt.UnixMilli()})
	notice := &whalereinv1.HostRequest{RequestId: "host-b-old-commit", HostId: op.HostID, DeviceId: op.DeviceID, Kind: whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE, Body: body, ProtocolVersion: 1, ExpiresAtUnixMs: op.ExpiresAt.UnixMilli()}
	if err := s.RevokeGrant(ctx, "host-b", "device", now); err != nil {
		t.Fatal(err)
	}
	if _, err := s.CommitPairing(ctx, hash, op.HostID, op.DeviceID, fixtureKeyPin(4), bytes.Repeat([]byte{9}, 32), now, notice, &op); !errors.Is(err, store.ErrPairingFenced) {
		t.Fatal("revoked Host accepted old FINISH")
	}
	if active, err := s.HasActiveGrant(ctx, "host-a", "device"); err != nil || !active {
		t.Fatal("unrelated Host grant changed")
	}
	if active, err := s.HasActiveGrant(ctx, "host-b", "device"); err != nil || active {
		t.Fatal("revoked Host grant was restored")
	}
	generation, err := s.HostGeneration(ctx, "host-b", "device")
	if err != nil || generation != 1 {
		t.Fatal("Host fence not advanced")
	}
	hash2 := bytes.Repeat([]byte{7}, 32)
	if err := s.CreatePairingToken(ctx, hash2, "host-b", now.Add(time.Hour), now); err != nil {
		t.Fatal(err)
	}
	op2 := store.PairingOperation{ClaimID: "host-b-pending-repeated", TokenHash: hash2, HostID: "host-b", DeviceID: "device", HostGeneration: 1, HelloHash: bytes.Repeat([]byte{2}, 32), HelloResponse: []byte("fixture"), ExpiresAt: now.Add(time.Hour), FinishHash: bytes.Repeat([]byte{3}, 32)}
	if err := s.SavePairingOperation(ctx, op2); err != nil {
		t.Fatal(err)
	}
	op2.FinishResponse, _ = proto.Marshal(&whalereinv1.PairingClaimResponse{ClaimId: op2.ClaimID, HostId: op2.HostID, DeviceId: op2.DeviceID, Phase: whalereinv1.PairingPhase_PAIRING_PHASE_FINISH, DevicePublicKey: pub, ProtocolVersion: 1})
	body2, _ := proto.Marshal(&whalereinv1.PairingClaim{ClaimId: op2.ClaimID, DeviceId: op2.DeviceID, DevicePublicKey: pub, Phase: whalereinv1.PairingPhase_PAIRING_PHASE_COMMIT, ExpiresAtUnixMs: op2.ExpiresAt.UnixMilli()})
	notice2 := &whalereinv1.HostRequest{RequestId: "host-b-repeat-commit", HostId: op2.HostID, DeviceId: op2.DeviceID, Kind: whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE, Body: body2, ProtocolVersion: 1, ExpiresAtUnixMs: op2.ExpiresAt.UnixMilli()}
	if err := s.RevokeGrant(ctx, "host-b", "device", now); err != nil {
		t.Fatal(err)
	}
	if _, err := s.CommitPairing(ctx, hash2, op2.HostID, op2.DeviceID, fixtureKeyPin(4), bytes.Repeat([]byte{9}, 32), now, notice2, &op2); !errors.Is(err, store.ErrPairingFenced) {
		t.Fatal("repeated revoke did not fence pending FINISH")
	}
	generation, err = s.HostGeneration(ctx, "host-a", "device")
	if err != nil || generation != 0 {
		t.Fatal("unrelated Host fence changed")
	}
}
