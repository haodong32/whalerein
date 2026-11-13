package store

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"testing"
	"time"

	"github.com/jackc/pgx/v5"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

type proofContractTx struct {
	pgx.Tx
	row pgx.Row
}

func (tx proofContractTx) QueryRow(context.Context, string, ...any) pgx.Row { return tx.row }

// A persisted binding cannot authenticate a different pairing, key, generation,
// protocol, or a COMMIT whose lifetime is shorter than its outbox lifetime.
func TestPersistedCommitProofRejectsContradictoryEvidence(t *testing.T) {
	at := time.Unix(1700000000, 0).UTC()
	key := make([]byte, 32)
	key[0] = 1
	pin := sha256.Sum256(key)
	for _, tc := range []struct {
		name   string
		mutate func(*whalereinv1.PairingClaim, *string, *uint32, *time.Time)
		want   bool
	}{
		{"valid", func(*whalereinv1.PairingClaim, *string, *uint32, *time.Time) {}, true},
		{"authorize phase", func(c *whalereinv1.PairingClaim, _ *string, _ *uint32, _ *time.Time) {
			c.Phase = whalereinv1.PairingPhase_PAIRING_PHASE_AUTHORIZE
		}, false},
		{"different claim", func(c *whalereinv1.PairingClaim, _ *string, _ *uint32, _ *time.Time) { c.ClaimId = "other" }, false},
		{"different device", func(c *whalereinv1.PairingClaim, _ *string, _ *uint32, _ *time.Time) { c.DeviceId = "other" }, false},
		{"token present", func(c *whalereinv1.PairingClaim, _ *string, _ *uint32, _ *time.Time) { c.TokenHash = "unexpected" }, false},
		{"different generation", func(c *whalereinv1.PairingClaim, _ *string, _ *uint32, _ *time.Time) { c.GrantGeneration++ }, false},
		{"different key", func(c *whalereinv1.PairingClaim, _ *string, _ *uint32, _ *time.Time) {
			c.DevicePublicKey = make([]byte, 32)
		}, false},
		{"wrong key pin", func(_ *whalereinv1.PairingClaim, p *string, _ *uint32, _ *time.Time) {
			other := sha256.Sum256([]byte("different device key"))
			*p = hex.EncodeToString(other[:])
		}, false},
		{"different protocol", func(_ *whalereinv1.PairingClaim, _ *string, p *uint32, _ *time.Time) { *p = 2 }, false},
		{"expired before outbox", func(c *whalereinv1.PairingClaim, _ *string, _ *uint32, _ *time.Time) { c.ExpiresAtUnixMs-- }, false},
		{"empty lifetime", func(_ *whalereinv1.PairingClaim, _ *string, _ *uint32, e *time.Time) { *e = at }, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			expiry := at.Add(time.Minute)
			fingerprint := hex.EncodeToString(pin[:])
			protocol := uint32(1)
			claim := &whalereinv1.PairingClaim{Phase: whalereinv1.PairingPhase_PAIRING_PHASE_COMMIT, ClaimId: "claim", DeviceId: "device", GrantGeneration: 3, DevicePublicKey: append([]byte(nil), key...), ExpiresAtUnixMs: expiry.UnixMilli()}
			tc.mutate(claim, &fingerprint, &protocol, &expiry)
			body, err := proto.Marshal(claim)
			if err != nil {
				t.Fatal(err)
			}
			tx := proofContractTx{row: confirmTestRow{values: []any{body, uint64(3), fingerprint, protocol, at, expiry}}}
			got, err := verifyPersistedCommitTx(context.Background(), tx, "claim", "host", "device", "request", key, 1)
			if err != nil || got != tc.want {
				t.Fatalf("proof accepted=%v, want %v, error=%v", got, tc.want, err)
			}
		})
	}
}
