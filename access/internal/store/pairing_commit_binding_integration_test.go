//go:build integration

package store_test

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
	"github.com/whalerein/whalerein/access/internal/store/storetest"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

// A non-COMMIT or mismatched peer identity cannot consume an invitation or
// create grant/outbox state; a subsequent correctly bound COMMIT still works.
func TestPairingCommitRejectsUnboundOutboxWithoutConsumingInvitation(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	now := time.Now().UTC()
	if _, err := s.EnsureHost(ctx, "host"); err != nil {
		t.Fatal(err)
	}
	hash := bytes.Repeat([]byte{4}, 32)
	if err := s.CreatePairingToken(ctx, hash, "host", now.Add(time.Minute), now); err != nil {
		t.Fatal(err)
	}
	op, notice := fixturePairingOperation(t, s, hash, "host", "device", "claim", now, nil)
	var original whalereinv1.PairingClaim
	if err := proto.Unmarshal(notice.Body, &original); err != nil {
		t.Fatal(err)
	}
	keyHash := sha256.Sum256(original.DevicePublicKey)
	fingerprint := hex.EncodeToString(keyHash[:])
	authHash := bytes.Repeat([]byte{5}, 32)
	assertUntouched := func(t *testing.T) {
		t.Helper()
		token, err := s.PairingTokenByHash(ctx, hash)
		if err != nil || token.UsedAt != nil {
			t.Fatal("invalid input consumed the invitation")
		}
		if _, err := s.DeviceByID(ctx, "device"); !errors.Is(err, store.ErrNotFound) {
			t.Fatal("invalid input persisted device identity")
		}
		var credentialExists bool
		if err := s.Pool().QueryRow(ctx, `SELECT EXISTS(SELECT 1 FROM device_credentials WHERE auth_hash=$1)`, authHash).Scan(&credentialExists); err != nil || credentialExists {
			t.Fatal("invalid input persisted a credential")
		}
		if active, err := s.HasActiveGrant(ctx, "host", "device"); err != nil || active {
			t.Fatal("invalid input changed authorization")
		}
		if _, err := s.GetRequest(ctx, notice.RequestId); !errors.Is(err, store.ErrNotFound) {
			t.Fatal("invalid input persisted an outbox request")
		}
		if _, err := s.PairingOutcome(ctx, "claim", "device"); !errors.Is(err, store.ErrNotFound) {
			t.Fatal("invalid input persisted a pairing outcome")
		}
	}
	t.Run("unbound device fingerprint", func(t *testing.T) {
		if accepted, err := s.CommitPairing(ctx, hash, "host", "device", strings.Repeat("0", 64), authHash, now, notice, &op); accepted || !errors.Is(err, store.ErrConflict) {
			t.Fatal("mismatched device fingerprint was accepted")
		}
		assertUntouched(t)
	})
	for _, tc := range []struct {
		name   string
		change func(*whalereinv1.PairingClaim)
	}{
		{"authorize", func(c *whalereinv1.PairingClaim) { c.Phase = whalereinv1.PairingPhase_PAIRING_PHASE_AUTHORIZE }},
		{"other device", func(c *whalereinv1.PairingClaim) { c.DeviceId = "other" }},
		{"other key", func(c *whalereinv1.PairingClaim) { c.DevicePublicKey[0] ^= 1 }},
		{"invitation hash", func(c *whalereinv1.PairingClaim) { c.TokenHash = strings.Repeat("a", 64) }},
	} {
		t.Run(tc.name, func(t *testing.T) {
			claim := proto.Clone(&original).(*whalereinv1.PairingClaim)
			tc.change(claim)
			candidate := proto.Clone(notice).(*whalereinv1.HostRequest)
			var err error
			candidate.Body, err = proto.Marshal(claim)
			if err != nil {
				t.Fatal(err)
			}
			if accepted, err := s.CommitPairing(ctx, hash, "host", "device", fingerprint, authHash, now, candidate, &op); accepted || !errors.Is(err, store.ErrConflict) {
				t.Fatal("invalid outbox claim was accepted")
			}
			assertUntouched(t)
		})
	}
	for _, tc := range []struct {
		name   string
		change func(*whalereinv1.HostRequest, *whalereinv1.PairingClaim)
	}{
		{"missing request id", func(n *whalereinv1.HostRequest, _ *whalereinv1.PairingClaim) { n.RequestId = "" }},
		{"expired outbox", func(n *whalereinv1.HostRequest, _ *whalereinv1.PairingClaim) { n.ExpiresAtUnixMs = now.UnixMilli() }},
		{"missing protocol", func(n *whalereinv1.HostRequest, _ *whalereinv1.PairingClaim) { n.ProtocolVersion = 0 }},
		{"different protocol", func(n *whalereinv1.HostRequest, _ *whalereinv1.PairingClaim) { n.ProtocolVersion = 2 }},
		{"claim expires first", func(_ *whalereinv1.HostRequest, c *whalereinv1.PairingClaim) { c.ExpiresAtUnixMs-- }},
	} {
		t.Run(tc.name, func(t *testing.T) {
			claim := proto.Clone(&original).(*whalereinv1.PairingClaim)
			candidate := proto.Clone(notice).(*whalereinv1.HostRequest)
			tc.change(candidate, claim)
			var err error
			candidate.Body, err = proto.Marshal(claim)
			if err != nil {
				t.Fatal(err)
			}
			if accepted, err := s.CommitPairing(ctx, hash, "host", "device", fingerprint, authHash, now, candidate, &op); accepted || !errors.Is(err, store.ErrConflict) {
				t.Fatal("undeliverable outbox was accepted")
			}
			assertUntouched(t)
		})
	}
	if accepted, err := s.CommitPairing(ctx, hash, "host", "device", fingerprint, authHash, now, notice, &op); err != nil || !accepted {
		t.Fatal("valid bound COMMIT did not succeed after invalid attempts")
	}
	if active, err := s.HasActiveGrant(ctx, "host", "device"); err != nil || !active {
		t.Fatal("valid pairing did not create the intended grant")
	}
}
