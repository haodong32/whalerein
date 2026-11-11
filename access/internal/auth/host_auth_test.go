package auth

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"encoding/hex"
	"errors"
	"testing"
	"time"

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
