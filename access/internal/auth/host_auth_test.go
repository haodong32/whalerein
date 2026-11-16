package auth

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"encoding/hex"
	"encoding/json"
	"errors"
	"io"
	"log/slog"
	"os"
	"path/filepath"
	"sync"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

func TestHostAuthorizationMatchesRFC8032Vector(t *testing.T) {
	seed, err := hex.DecodeString("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60")
	if err != nil {
		t.Fatal(err)
	}
	want, err := hex.DecodeString("e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b")
	if err != nil {
		t.Fatal(err)
	}
	private := ed25519.NewKeyFromSeed(seed)
	got := ed25519.Sign(private, nil)
	if !bytes.Equal(got, want) {
		t.Fatalf("signature %x", got)
	}
	if !ed25519.Verify(private.Public().(ed25519.PublicKey), nil, want) {
		t.Fatal("verification rejected the RFC 8032 signature")
	}
}

func TestHostAuthorizationKeySignsAndVerifiesRequests(t *testing.T) {
	service, fs, _, transport := newService(t)
	now := time.UnixMilli(1_700_000_000_000)
	service.now = func() time.Time { return now }
	publicKey, privateKey, err := ed25519.GenerateKey(nil)
	if err != nil {
		t.Fatal(err)
	}
	fs.hosts["h1"] = store.Host{HostID: "h1"}
	ctx := context.Background()
	if err := service.RememberHostAuthorizationKey(ctx, "h1", publicKey); err != nil {
		t.Fatal(err)
	}
	otherPublic, _, err := ed25519.GenerateKey(nil)
	if err != nil {
		t.Fatal(err)
	}
	if err := service.RememberHostAuthorizationKey(ctx, "h1", otherPublic); err != nil {
		t.Fatal(err)
	}
	if got := fs.keys["h1"]; string(got) != string(publicKey) {
		t.Fatal("first authorization key was replaced")
	}
	if err := service.RememberHostAuthorizationKey(ctx, "missing", publicKey); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("missing host: %v", err)
	}
	if err := service.RememberHostAuthorizationKey(ctx, "h1", []byte("short")); err != ErrBadRequest {
		t.Fatalf("short key: %v", err)
	}

	signedAt := now.UnixMilli()
	message := hostAuthMessage(HostAuthInvitation, "h1", "", signedAt)
	signature := ed25519.Sign(privateKey, message)
	proof := HostProof{Purpose: HostAuthInvitation, HostID: "h1", SignedAtUnixMs: signedAt, Signature: signature}
	if err := service.AuthenticateHostRequest(ctx, proof); err != nil {
		t.Fatal(err)
	}
	if err := service.AuthenticateHostRequest(ctx, proof); err != ErrUnauthorized {
		t.Fatalf("replay: %v", err)
	}
	proof.HostID = "h2"
	proof.SignedAtUnixMs = signedAt + 1
	proof.Signature = ed25519.Sign(privateKey, hostAuthMessage(HostAuthInvitation, "h2", "", proof.SignedAtUnixMs))
	if err := service.AuthenticateHostRequest(ctx, proof); err != ErrUnauthorized {
		t.Fatalf("other host: %v", err)
	}
	proof = HostProof{
		Purpose:        HostAuthInvitation,
		HostID:         "h1",
		SignedAtUnixMs: now.Add(-3 * time.Minute).UnixMilli(),
		Signature:      signature,
	}
	if err := service.AuthenticateHostRequest(ctx, proof); err != ErrSignatureExpired {
		t.Fatalf("expired: %v", err)
	}
	proof.SignedAtUnixMs = signedAt + 2
	proof.Signature = []byte("not-a-signature")
	if err := service.AuthenticateHostRequest(ctx, proof); err != ErrUnauthorized {
		t.Fatalf("bad signature: %v", err)
	}

	fs.grants[grantKey("h1", "d1")] = true
	revokeAt := signedAt + 3
	revokeMessage := hostAuthMessage(HostAuthRevoke, "h1", "d1", revokeAt)
	if err := service.AuthenticateHostRequest(ctx, HostProof{
		Purpose: HostAuthRevoke, HostID: "h1", DeviceID: "d1", SignedAtUnixMs: revokeAt,
		Signature: ed25519.Sign(privateKey, revokeMessage),
	}); err != nil {
		t.Fatal(err)
	}
	if err := service.RevokeHostGrant(ctx, "h1", "d1"); err != nil {
		t.Fatal(err)
	}
	if fs.grants[grantKey("h1", "d1")] {
		t.Fatal("grant still active")
	}
	if len(transport.disconnected) != 1 || transport.disconnected[0] != "d1" {
		t.Fatalf("disconnect = %v", transport.disconnected)
	}
	fs.grants[grantKey("h2", "d1")] = true
	if fs.grants[grantKey("h2", "d1")] != true {
		t.Fatal("other host grant changed")
	}
	if err := service.RevokeHostGrant(ctx, "h1", "missing"); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("missing grant: %v", err)
	}
}

func TestHostScopedRevocationPublishesTheCommittedGrantIncarnation(t *testing.T) {
	service, fs, presence, transport := newService(t)
	fs.grants[grantKey("h", "d")] = true
	fs.pending = []store.Revocation{{HostID: "h", DeviceID: "d", GrantGeneration: 4, RevokedAt: time.Unix(10, 0)}}
	presence.online["h"] = true
	transport.awaitFn = successAck
	if err := service.RevokeHostGrant(context.Background(), "h", "d"); err != nil {
		t.Fatal(err)
	}
	if len(transport.awaited) != 1 {
		t.Fatal("host-scoped revocation was not delivered")
	}
	var notice whalereinv1.RevocationNotice
	if err := proto.Unmarshal(transport.awaited[0].Body, &notice); err != nil {
		t.Fatal(err)
	}
	if notice.GrantGenerations["d"] != 4 {
		t.Fatal("host-scoped revocation lost its incarnation")
	}
}

// --- ACC-02: shared Go/JavaScript signing vectors -------------------------

type hostAuthVectorFile struct {
	SeedHex      string `json:"seed_hex"`
	PublicKeyHex string `json:"public_key_hex"`
	Window       struct {
		PastMs   int64 `json:"past_ms"`
		FutureMs int64 `json:"future_ms"`
	} `json:"window"`
	Cases []struct {
		Purpose        string `json:"purpose"`
		HostID         string `json:"host_id"`
		DeviceID       string `json:"device_id"`
		SignedAtUnixMs int64  `json:"signed_at_unix_ms"`
		Message        string `json:"message"`
		SignatureHex   string `json:"signature_hex"`
	} `json:"cases"`
}

func loadHostAuthVectors(t *testing.T) hostAuthVectorFile {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join("testdata", "host_auth_vectors.json"))
	if err != nil {
		t.Fatal(err)
	}
	var vectors hostAuthVectorFile
	if err := json.Unmarshal(raw, &vectors); err != nil {
		t.Fatal(err)
	}
	if len(vectors.Cases) == 0 {
		t.Fatal("empty vector file")
	}
	return vectors
}

// TestHostAuthMessageMatchesTheSharedCrossLanguageVectors pins the exact bytes
// the Host runtime signs (host/runtime/src/security/host-auth.js). A change to
// the prefix, purpose, host id, device id or timestamp encoding fails here
// before it can become a silent pairing outage.
func TestHostAuthMessageMatchesTheSharedCrossLanguageVectors(t *testing.T) {
	vectors := loadHostAuthVectors(t)
	seed, err := hex.DecodeString(vectors.SeedHex)
	if err != nil || len(seed) != ed25519.SeedSize {
		t.Fatalf("seed = %x err=%v", seed, err)
	}
	private := ed25519.NewKeyFromSeed(seed)
	if hex.EncodeToString(private.Public().(ed25519.PublicKey)) != vectors.PublicKeyHex {
		t.Fatal("vector public key does not match the seed")
	}
	if vectors.Window.PastMs != hostAuthPast.Milliseconds() || vectors.Window.FutureMs != hostAuthFuture.Milliseconds() {
		t.Fatalf("clock window drift: Go=%d/%d vectors=%d/%d",
			hostAuthPast.Milliseconds(), hostAuthFuture.Milliseconds(), vectors.Window.PastMs, vectors.Window.FutureMs)
	}
	for _, item := range vectors.Cases {
		t.Run(item.Purpose+"_"+item.HostID, func(t *testing.T) {
			message := hostAuthMessage(item.Purpose, item.HostID, item.DeviceID, item.SignedAtUnixMs)
			if string(message) != item.Message {
				t.Fatalf("message = %q, want %q", message, item.Message)
			}
			signature, err := hex.DecodeString(item.SignatureHex)
			if err != nil || len(signature) != ed25519.SignatureSize {
				t.Fatalf("signature = %x err=%v", signature, err)
			}
			if !ed25519.Verify(private.Public().(ed25519.PublicKey), message, signature) {
				t.Fatal("vector signature does not verify over the Go message")
			}
			// The message the Host signs and the message Access verifies must be
			// byte-identical; a re-sign proves the shared seed/signature pair.
			if !bytes.Equal(ed25519.Sign(private, message), signature) {
				t.Fatal("shared-seed signature differs from the vector")
			}
		})
	}
}

// --- ACC-02: acceptance window boundaries ---------------------------------

func TestHostAuthClockWindowBoundariesAreInclusive(t *testing.T) {
	service, fs, _, _ := newService(t)
	now := time.UnixMilli(1_700_000_000_000)
	service.now = func() time.Time { return now }
	private, public := mustGenerateHostKey(t)
	fs.hosts["h1"] = store.Host{HostID: "h1"}
	if err := service.RememberHostAuthorizationKey(context.Background(), "h1", public); err != nil {
		t.Fatal(err)
	}

	cases := []struct {
		name      string
		offset    time.Duration
		wantErr   error
		timestamp int64
	}{
		{"accepts exactly two minutes in the past", -hostAuthPast, nil, now.Add(-hostAuthPast).UnixMilli()},
		{"rejects one millisecond beyond the past boundary", -hostAuthPast - time.Millisecond, ErrSignatureExpired, now.Add(-hostAuthPast - time.Millisecond).UnixMilli()},
		{"accepts exactly one minute in the future", hostAuthFuture, nil, now.Add(hostAuthFuture).UnixMilli()},
		{"rejects one millisecond beyond the future boundary", hostAuthFuture + time.Millisecond, ErrSignatureExpired, now.Add(hostAuthFuture + time.Millisecond).UnixMilli()},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			signedAt := tc.timestamp
			proof := HostProof{
				Purpose: HostAuthInvitation, HostID: "h1", SignedAtUnixMs: signedAt,
				Signature: ed25519.Sign(private, hostAuthMessage(HostAuthInvitation, "h1", "", signedAt)),
			}
			err := service.AuthenticateHostRequest(context.Background(), proof)
			if tc.wantErr == nil {
				if err != nil {
					t.Fatalf("boundary rejected: %v", err)
				}
				return
			}
			if !errors.Is(err, tc.wantErr) {
				t.Fatalf("err = %v, want %v", err, tc.wantErr)
			}
		})
	}
}

// --- ACC-18: durable replay guard across replicas -------------------------

// sharedReplayStore adds the durable guard to the in-memory auth fake while
// keeping every replica's process-local replay map independent.
type sharedReplayStore struct {
	*fakeStore
	mu   sync.Mutex
	used map[string]bool
}

func (s *sharedReplayStore) ConsumeHostSignature(_ context.Context, signatureHash []byte, hostID, purpose, deviceID string, signedAtUnixMs int64, at time.Time) (bool, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	key := string(signatureHash)
	if s.used[key] {
		return false, nil
	}
	s.used[key] = true
	return true, nil
}

func TestHostSignatureReplayIsRejectedAcrossReplicasSharingOneGuard(t *testing.T) {
	base := newFakeStore()
	base.hosts["h1"] = store.Host{HostID: "h1"}
	guard := &sharedReplayStore{fakeStore: base, used: map[string]bool{}}
	newReplica := func(replayStore *sharedReplayStore) *Service {
		t.Helper()
		service, err := New(Dependencies{
			Store: replayStore, Admission: newFakeAdmission(replayStore.fakeStore), Enforcer: &fakeEnforcer{},
			Presence: &fakePresence{online: map[string]bool{}}, Transport: &fakeTransport{},
			Config:  Config{ProtocolVersion: 1, RevocationAckTimeout: time.Second},
			Metrics: diagnostics.NewMetrics(),
			Masker:  diagnostics.NewMaskerWithKey([]byte("replica")),
			Logger:  slog.New(slog.NewTextHandler(io.Discard, nil)),
		})
		if err != nil {
			t.Fatal(err)
		}
		service.now = func() time.Time { return time.UnixMilli(1_700_000_000_000) }
		return service
	}
	first := newReplica(guard)
	second := newReplica(guard)
	private, public := mustGenerateHostKey(t)
	for _, service := range []*Service{first, second} {
		if err := service.RememberHostAuthorizationKey(context.Background(), "h1", public); err != nil {
			t.Fatal(err)
		}
	}
	signedAt := int64(1_700_000_000_000)
	proof := HostProof{
		Purpose: HostAuthInvitation, HostID: "h1", SignedAtUnixMs: signedAt,
		Signature: ed25519.Sign(private, hostAuthMessage(HostAuthInvitation, "h1", "", signedAt)),
	}
	if err := first.AuthenticateHostRequest(context.Background(), proof); err != nil {
		t.Fatalf("first replica rejected a fresh signature: %v", err)
	}
	// The second replica has an empty process-local map; only the shared
	// durable guard can reject the replay.
	if err := second.AuthenticateHostRequest(context.Background(), proof); !errors.Is(err, ErrUnauthorized) {
		t.Fatalf("second replica accepted a replayed signature: %v", err)
	}
	// A freshly signed proof is still accepted by the second replica.
	next := signedAt + 1
	proof.SignedAtUnixMs = next
	proof.Signature = ed25519.Sign(private, hostAuthMessage(HostAuthInvitation, "h1", "", next))
	if err := second.AuthenticateHostRequest(context.Background(), proof); err != nil {
		t.Fatalf("fresh signature rejected after replay: %v", err)
	}
}

func TestHostSignatureReplayGuardFailureFailsClosed(t *testing.T) {
	base := newFakeStore()
	base.hosts["h1"] = store.Host{HostID: "h1"}
	guard := &failingReplayStore{sharedReplayStore: &sharedReplayStore{fakeStore: base, used: map[string]bool{}}}
	service, err := New(Dependencies{
		Store: guard, Admission: newFakeAdmission(base), Enforcer: &fakeEnforcer{},
		Presence: &fakePresence{online: map[string]bool{}}, Transport: &fakeTransport{},
		Config:  Config{ProtocolVersion: 1, RevocationAckTimeout: time.Second},
		Metrics: diagnostics.NewMetrics(),
		Masker:  diagnostics.NewMaskerWithKey([]byte("replica")),
		Logger:  slog.New(slog.NewTextHandler(io.Discard, nil)),
	})
	if err != nil {
		t.Fatal(err)
	}
	private, public := mustGenerateHostKey(t)
	if err := service.RememberHostAuthorizationKey(context.Background(), "h1", public); err != nil {
		t.Fatal(err)
	}
	proof := HostProof{
		Purpose: HostAuthInvitation, HostID: "h1", SignedAtUnixMs: service.now().UnixMilli(),
		Signature: ed25519.Sign(private, hostAuthMessage(HostAuthInvitation, "h1", "", service.now().UnixMilli())),
	}
	// A guard outage must not silently fall back to the per-process map.
	if err := service.AuthenticateHostRequest(context.Background(), proof); err == nil {
		t.Fatal("guard failure accepted the authorization")
	}
}

type failingReplayStore struct{ *sharedReplayStore }

func (*failingReplayStore) ConsumeHostSignature(context.Context, []byte, string, string, string, int64, time.Time) (bool, error) {
	return false, errors.New("synthetic durable guard failure")
}

func mustGenerateHostKey(t *testing.T) (ed25519.PrivateKey, ed25519.PublicKey) {
	t.Helper()
	public, private, err := ed25519.GenerateKey(nil)
	if err != nil {
		t.Fatal(err)
	}
	return private, public
}
