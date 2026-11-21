package auth

import (
	"context"
	"crypto/ed25519"
	"encoding/hex"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// The canonical texts are pinned literally: a runtime that changes a separator,
// a decimal rendering or the enum-name spelling must fail here first.
func TestRelaySignedTextVectors(t *testing.T) {
	digest := make([]byte, 32)
	for i := range digest {
		digest[i] = byte(i)
	}
	digestHex := hex.EncodeToString(digest)
	objectID := strings.Repeat("ab", 16)

	tests := []struct {
		name    string
		purpose string
		fields  RelayFields
		want    string
	}{
		{
			name:    "reserve with every scope field",
			purpose: RelayAuthReserve,
			fields: RelayFields{
				RelayVersion: 3, HostID: "host-1", DeviceID: "device-1",
				SessionID: "session-1", ChannelID: "channel-1",
				Purpose:         whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_ATTACHMENT_PART,
				PartID:          "part-1",
				CiphertextBytes: 12, SHA256: digest,
				ExpiresAtUnixMs: 1_700_000_060_000, SignedAtUnixMs: 1_700_000_000_000,
			},
			want: "whalerein/v1/relay|reserve|3|host-1|device-1|session-1|channel-1|PAYLOAD_PURPOSE_ATTACHMENT_PART|part-1|12|" + digestHex + "|1700000060000|1700000000000",
		},
		{
			name:    "reserve with empty session channel and part",
			purpose: RelayAuthReserve,
			fields: RelayFields{
				RelayVersion: 3, HostID: "h", DeviceID: "d",
				Purpose:         whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_RESULT,
				CiphertextBytes: 7, SHA256: digest,
				ExpiresAtUnixMs: 2, SignedAtUnixMs: 1,
			},
			want: "whalerein/v1/relay|reserve|3|h|d|||PAYLOAD_PURPOSE_RESULT||7|" + digestHex + "|2|1",
		},
		{
			name:    "upload",
			purpose: RelayAuthUpload,
			fields: RelayFields{
				RelayVersion: 3, HostID: "h", ObjectID: objectID,
				CiphertextBytes: 42, SHA256: digest, SignedAtUnixMs: 9,
			},
			want: "whalerein/v1/relay|upload|3|" + objectID + "|42|" + digestHex + "|9",
		},
		{
			name:    "finish",
			purpose: RelayAuthFinish,
			fields: RelayFields{
				RelayVersion: 3, HostID: "h", ObjectID: objectID,
				CiphertextBytes: 42, SHA256: digest, SignedAtUnixMs: 9,
			},
			want: "whalerein/v1/relay|finish|3|" + objectID + "|42|" + digestHex + "|9",
		},
		{
			name:    "object",
			purpose: RelayAuthObject,
			fields:  RelayFields{RelayVersion: 3, HostID: "h", ObjectID: objectID, SignedAtUnixMs: 5},
			want:    "whalerein/v1/relay|object|3|" + objectID + "|5",
		},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			got, err := RelaySignedText(test.purpose, test.fields)
			if err != nil {
				t.Fatal(err)
			}
			if string(got) != test.want {
				t.Fatalf("signed text\n got: %q\nwant: %q", got, test.want)
			}
		})
	}
}

func TestRelaySignedTextRejectsAmbiguousFields(t *testing.T) {
	digest := make([]byte, 32)
	objectID := strings.Repeat("ab", 16)
	base := RelayFields{
		RelayVersion: 3, HostID: "h", DeviceID: "d",
		Purpose:         whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_REQUEST,
		CiphertextBytes: 1, SHA256: digest, ExpiresAtUnixMs: 2, SignedAtUnixMs: 1,
	}
	with := func(mutate func(*RelayFields)) RelayFields {
		fields := base
		mutate(&fields)
		return fields
	}
	cases := []struct {
		name    string
		purpose string
		fields  RelayFields
	}{
		{name: "pipe in device id", purpose: RelayAuthReserve, fields: with(func(f *RelayFields) { f.DeviceID = "d|e" })},
		{name: "newline in part id", purpose: RelayAuthReserve, fields: with(func(f *RelayFields) { f.PartID = "p\nq" })},
		{name: "missing device", purpose: RelayAuthReserve, fields: with(func(f *RelayFields) { f.DeviceID = "" })},
		{name: "missing host", purpose: RelayAuthReserve, fields: with(func(f *RelayFields) { f.HostID = "" })},
		{name: "unspecified purpose", purpose: RelayAuthReserve, fields: with(func(f *RelayFields) { f.Purpose = 0 })},
		{name: "short digest", purpose: RelayAuthReserve, fields: with(func(f *RelayFields) { f.SHA256 = digest[:31] })},
		{name: "oversize host", purpose: RelayAuthReserve, fields: with(func(f *RelayFields) { f.HostID = strings.Repeat("h", 201) })},
		{name: "bad object id", purpose: RelayAuthUpload, fields: RelayFields{RelayVersion: 3, HostID: "h", ObjectID: strings.Repeat("AB", 16), SHA256: digest}},
		{name: "short object id", purpose: RelayAuthObject, fields: RelayFields{RelayVersion: 3, HostID: "h", ObjectID: "abc"}},
		{name: "unknown purpose", purpose: "delete", fields: RelayFields{RelayVersion: 3, HostID: "h", ObjectID: objectID}},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			if _, err := RelaySignedText(test.purpose, test.fields); !errors.Is(err, ErrBadRequest) {
				t.Fatalf("err = %v", err)
			}
		})
	}
}

func TestAuthenticateRelayRequestVerifiesWindowAndReplay(t *testing.T) {
	service, fs, _, _ := newService(t)
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

	digest := make([]byte, 32)
	objectID := strings.Repeat("cd", 16)
	fields := RelayFields{
		RelayVersion: 3, HostID: "h1", ObjectID: objectID,
		CiphertextBytes: 4, SHA256: digest, SignedAtUnixMs: now.UnixMilli(),
	}
	message, err := RelaySignedText(RelayAuthObject, fields)
	if err != nil {
		t.Fatal(err)
	}
	signature := ed25519.Sign(privateKey, message)
	if err := service.AuthenticateRelayRequest(ctx, RelayAuthObject, fields, signature); err != nil {
		t.Fatal(err)
	}
	// The same signature is consumed exactly once.
	if err := service.AuthenticateRelayRequest(ctx, RelayAuthObject, fields, signature); !errors.Is(err, ErrUnauthorized) {
		t.Fatalf("replay: %v", err)
	}

	// A different operation cannot reuse an object signature.
	finishFields := fields
	if err := service.AuthenticateRelayRequest(ctx, RelayAuthFinish, finishFields, signature); !errors.Is(err, ErrUnauthorized) {
		t.Fatalf("purpose confusion: %v", err)
	}

	// Outside the two-minute past / one-minute future window.
	fresh := fields
	fresh.SignedAtUnixMs = now.Add(-3 * time.Minute).UnixMilli()
	freshMessage, _ := RelaySignedText(RelayAuthObject, fresh)
	if err := service.AuthenticateRelayRequest(ctx, RelayAuthObject, fresh, ed25519.Sign(privateKey, freshMessage)); !errors.Is(err, ErrSignatureExpired) {
		t.Fatalf("past window: %v", err)
	}
	fresh.SignedAtUnixMs = now.Add(2 * time.Minute).UnixMilli()
	freshMessage, _ = RelaySignedText(RelayAuthObject, fresh)
	if err := service.AuthenticateRelayRequest(ctx, RelayAuthObject, fresh, ed25519.Sign(privateKey, freshMessage)); !errors.Is(err, ErrSignatureExpired) {
		t.Fatalf("future window: %v", err)
	}
	fresh.SignedAtUnixMs = now.Add(30 * time.Second).UnixMilli()
	freshMessage, _ = RelaySignedText(RelayAuthObject, fresh)
	if err := service.AuthenticateRelayRequest(ctx, RelayAuthObject, fresh, ed25519.Sign(privateKey, freshMessage)); err != nil {
		t.Fatalf("inside future window: %v", err)
	}
}

func TestAuthenticateRelayRequestRejectsBadKeysAndShapes(t *testing.T) {
	service, fs, _, _ := newService(t)
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
	fields := RelayFields{
		RelayVersion: 3, HostID: "h1", ObjectID: strings.Repeat("cd", 16),
		SignedAtUnixMs: now.UnixMilli(),
	}
	message, _ := RelaySignedText(RelayAuthObject, fields)
	good := ed25519.Sign(privateKey, message)

	if err := service.AuthenticateRelayRequest(ctx, RelayAuthObject, fields, nil); !errors.Is(err, ErrUnauthorized) {
		t.Fatalf("nil signature: %v", err)
	}
	if err := service.AuthenticateRelayRequest(ctx, RelayAuthObject, fields, good[:10]); !errors.Is(err, ErrUnauthorized) {
		t.Fatalf("short signature: %v", err)
	}
	if err := service.AuthenticateRelayRequest(ctx, RelayAuthObject, fields, make([]byte, ed25519.SignatureSize)); !errors.Is(err, ErrUnauthorized) {
		t.Fatalf("zero signature: %v", err)
	}
	missing := fields
	missing.HostID = "missing"
	if err := service.AuthenticateRelayRequest(ctx, RelayAuthObject, missing, good); !errors.Is(err, ErrUnauthorized) {
		t.Fatalf("unknown host: %v", err)
	}
	// Invitation and revocation remain separate authorities.
	if err := service.AuthenticateHostRequest(ctx, HostProof{
		Purpose: RelayAuthObject, HostID: "h1", SignedAtUnixMs: fields.SignedAtUnixMs, Signature: good,
	}); !errors.Is(err, ErrUnauthorized) {
		t.Fatalf("relay purpose must not authenticate a pairing request: %v", err)
	}
}

func TestRelayReplayPurposeNamespace(t *testing.T) {
	if relayReplayPurpose(RelayAuthObject) == HostAuthInvitation {
		t.Fatal("relay replay namespace collides with invitation")
	}
	if relayReplayBinding(RelayAuthReserve, RelayFields{DeviceID: "d", CiphertextBytes: 3}) != "d:3" {
		t.Fatal("reserve replay binding")
	}
	if relayReplayBinding(RelayAuthObject, RelayFields{ObjectID: "o"}) != "o" {
		t.Fatal("object replay binding")
	}
}
