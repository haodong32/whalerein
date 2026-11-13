package store

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/jackc/pgx/v5"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

type boundCommitFixture struct {
	op                  PairingOperation
	response            *whalereinv1.PairingClaimResponse
	claim               *whalereinv1.PairingClaim
	notice              *whalereinv1.HostRequest
	tokenHash, authHash []byte
	fingerprint         string
	now                 time.Time
}

func newBoundCommitFixture(t *testing.T) *boundCommitFixture {
	t.Helper()
	now := time.Now().UTC()
	key := bytes.Repeat([]byte{3}, 32)
	keyHash := sha256.Sum256(key)
	tokenHash, authHash := bytes.Repeat([]byte{4}, 32), bytes.Repeat([]byte{5}, 32)
	response := &whalereinv1.PairingClaimResponse{ClaimId: "claim", HostId: "host", DeviceId: "device", Phase: whalereinv1.PairingPhase_PAIRING_PHASE_FINISH, DevicePublicKey: append([]byte(nil), key...), ExpiresAtUnixMs: now.Add(time.Minute).UnixMilli(), ProtocolVersion: 1}
	claim := &whalereinv1.PairingClaim{ClaimId: "claim", DeviceId: "device", Phase: whalereinv1.PairingPhase_PAIRING_PHASE_COMMIT, DevicePublicKey: append([]byte(nil), key...), ExpiresAtUnixMs: now.Add(time.Minute).UnixMilli()}
	f := &boundCommitFixture{response: response, claim: claim, tokenHash: tokenHash, authHash: authHash, fingerprint: hex.EncodeToString(keyHash[:]), now: now}
	f.notice = &whalereinv1.HostRequest{RequestId: "pair-commit-fixture", HostId: "host", DeviceId: "device", Kind: whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE, ProtocolVersion: 1, ExpiresAtUnixMs: now.Add(time.Minute).UnixMilli()}
	f.op = PairingOperation{ClaimID: "claim", HostID: "host", DeviceID: "device", TokenHash: tokenHash, HelloHash: bytes.Repeat([]byte{6}, 32), HelloResponse: []byte("opaque"), ExpiresAt: now.Add(time.Minute), FinishHash: bytes.Repeat([]byte{7}, 32)}
	f.encode(t)
	return f
}
func (f *boundCommitFixture) encode(t *testing.T) {
	t.Helper()
	var err error
	f.op.FinishResponse, err = proto.Marshal(f.response)
	if err != nil {
		t.Fatal(err)
	}
	f.notice.Body, err = proto.Marshal(f.claim)
	if err != nil {
		t.Fatal(err)
	}
}
func (f *boundCommitFixture) commit(begin func(context.Context) (pgx.Tx, error)) (bool, error) {
	return commitPairing(context.Background(), begin, f.tokenHash, "host", "device", f.fingerprint, f.authHash, f.now, f.notice, &f.op)
}

// The complete valid fixture reaches transaction admission; each subsequent
// case changes one field and must be rejected before any database operation.
func TestCommitPairingRejectsUnboundReceiptBeforeOpeningTransaction(t *testing.T) {
	valid := newBoundCommitFixture(t)
	began := false
	accepted, err := valid.commit(func(context.Context) (pgx.Tx, error) { began = true; return nil, errFenceTest })
	if accepted || !began || !errors.Is(err, errFenceTest) {
		t.Fatal("valid control did not reach transaction admission")
	}
	t.Run("missing operation", func(t *testing.T) {
		f := newBoundCommitFixture(t)
		began := false
		accepted, err := commitPairing(context.Background(), func(context.Context) (pgx.Tx, error) { began = true; return nil, errFenceTest }, f.tokenHash, "host", "device", f.fingerprint, f.authHash, f.now, f.notice, nil)
		if accepted || !errors.Is(err, ErrConflict) || began {
			t.Fatal("missing operation reached transaction admission")
		}
	})
	t.Run("invalid notice encoding", func(t *testing.T) {
		f := newBoundCommitFixture(t)
		f.notice.Body = []byte{0xff}
		began := false
		accepted, err := f.commit(func(context.Context) (pgx.Tx, error) { began = true; return nil, errFenceTest })
		if accepted || !errors.Is(err, ErrConflict) || began {
			t.Fatal("invalid notice reached transaction admission")
		}
	})
	for _, tc := range []struct {
		name   string
		change func(*boundCommitFixture)
	}{
		{"missing claim", func(f *boundCommitFixture) { f.op.ClaimID = "" }},
		{"receipt contains credential", func(f *boundCommitFixture) { f.response.DeviceToken = strings.Repeat("x", 4) }},
		{"wrong receipt claim", func(f *boundCommitFixture) { f.response.ClaimId = "other" }},
		{"wrong receipt host", func(f *boundCommitFixture) { f.response.HostId = "other" }},
		{"wrong receipt device", func(f *boundCommitFixture) { f.response.DeviceId = "other" }},
		{"wrong receipt phase", func(f *boundCommitFixture) { f.response.Phase = whalereinv1.PairingPhase_PAIRING_PHASE_HELLO }},
		{"invalid device key", func(f *boundCommitFixture) { f.response.DevicePublicKey = []byte{1} }},
		{"invalid finish hash", func(f *boundCommitFixture) { f.op.FinishHash = []byte{1} }},
	} {
		t.Run(tc.name, func(t *testing.T) {
			f := newBoundCommitFixture(t)
			tc.change(f)
			f.encode(t)
			began := false
			accepted, err := f.commit(func(context.Context) (pgx.Tx, error) { began = true; return nil, errFenceTest })
			if accepted || !errors.Is(err, ErrConflict) || began {
				t.Fatal("invalid receipt reached a grant transaction")
			}
		})
	}
}

// A grant cannot be reported Host-confirmed using an AUTHORIZE-only or
// differently scoped outbox message: those messages do not activate the peer.
func TestCommitPairingRejectsOutboxWithoutMatchingCommitClaim(t *testing.T) {
	for _, tc := range []struct {
		name   string
		change func(*boundCommitFixture)
	}{
		{"authorize rather than commit", func(f *boundCommitFixture) { f.claim.Phase = whalereinv1.PairingPhase_PAIRING_PHASE_AUTHORIZE }},
		{"different device", func(f *boundCommitFixture) { f.claim.DeviceId = "other" }},
		{"different device key", func(f *boundCommitFixture) { f.claim.DevicePublicKey[0] ^= 1 }},
		{"invitation hash retained", func(f *boundCommitFixture) { f.claim.TokenHash = strings.Repeat("a", 64) }},
		{"fingerprint does not match key", func(f *boundCommitFixture) { f.fingerprint = strings.Repeat("0", 64) }},
	} {
		t.Run(tc.name, func(t *testing.T) {
			f := newBoundCommitFixture(t)
			tc.change(f)
			f.encode(t)
			began := false
			accepted, err := f.commit(func(context.Context) (pgx.Tx, error) { began = true; return nil, errFenceTest })
			if accepted || !errors.Is(err, ErrConflict) || began {
				t.Fatal("invalid commit reached a grant transaction")
			}
		})
	}
}

// An outbox must remain addressable, deliverable before its deadline and
// compatible with the protocol version returned to the pairing client.
func TestCommitPairingRejectsInvalidOutboxMetadataBeforeTransaction(t *testing.T) {
	t.Run("claim deadline beyond outbox remains valid", func(t *testing.T) {
		f := newBoundCommitFixture(t)
		f.claim.ExpiresAtUnixMs++
		f.encode(t)
		began := false
		accepted, err := f.commit(func(context.Context) (pgx.Tx, error) { began = true; return nil, errFenceTest })
		if accepted || !began || !errors.Is(err, errFenceTest) {
			t.Fatal("deliverable outbox did not reach transaction admission")
		}
	})
	for _, tc := range []struct {
		name   string
		change func(*boundCommitFixture)
	}{
		{"missing request id", func(f *boundCommitFixture) { f.notice.RequestId = "" }},
		{"expired deadline", func(f *boundCommitFixture) { f.notice.ExpiresAtUnixMs = f.now.UnixMilli() }},
		{"missing protocol", func(f *boundCommitFixture) { f.notice.ProtocolVersion = 0 }},
		{"mismatched protocol", func(f *boundCommitFixture) { f.notice.ProtocolVersion = 2 }},
		{"claim expires before outbox", func(f *boundCommitFixture) { f.claim.ExpiresAtUnixMs-- }},
	} {
		t.Run(tc.name, func(t *testing.T) {
			f := newBoundCommitFixture(t)
			tc.change(f)
			f.encode(t)
			began := false
			accepted, err := f.commit(func(context.Context) (pgx.Tx, error) { began = true; return nil, errFenceTest })
			if accepted || !errors.Is(err, ErrConflict) || began {
				t.Fatal("undeliverable outbox reached a grant transaction")
			}
		})
	}
}

// Even after tentative writes, an outbox addressed to another authority must
// abort and must not expose a newly generated COMMIT body.
func TestCommitPairingRollsBackOutboxAuthorityMismatch(t *testing.T) {
	for _, tc := range []struct {
		name   string
		change func(*whalereinv1.HostRequest)
	}{
		{"host", func(n *whalereinv1.HostRequest) { n.HostId = "other" }},
		{"device", func(n *whalereinv1.HostRequest) { n.DeviceId = "other" }},
		{"kind", func(n *whalereinv1.HostRequest) { n.Kind = whalereinv1.RequestKind_REQUEST_KIND_QUERY }},
	} {
		t.Run(tc.name, func(t *testing.T) {
			f := newBoundCommitFixture(t)
			tc.change(f.notice)
			original := append([]byte(nil), f.notice.Body...)
			tx := &commitPairingTx{t: t, device: 2, host: 3}
			accepted, err := f.commit(func(context.Context) (pgx.Tx, error) { return tx, nil })
			if accepted || !errors.Is(err, ErrConflict) || !bytes.Equal(f.notice.Body, original) {
				t.Fatal("invalid outbox was accepted or changed")
			}
			if len(tx.steps) == 0 || tx.steps[len(tx.steps)-1] != "rollback" {
				t.Fatal("transaction was not rolled back")
			}
			for _, step := range tx.steps {
				if step == "commit" || step == "notice-write" || step == "outcome-write" {
					t.Fatalf("unexpected step %s", step)
				}
			}
		})
	}
}

// A failed transaction start cannot be interpreted as a committed grant,
// a proven fence, a confirmed Host, or an applied result.
func TestPairingRecoveryFailsClosedWhenTransactionCannotStart(t *testing.T) {
	begin := func(context.Context) (pgx.Tx, error) { return nil, errFenceTest }
	ctx := context.Background()
	if err := savePairingOperation(ctx, begin, PairingOperation{}); !errors.Is(err, errFenceTest) {
		t.Fatal("failed transaction reported saved HELLO")
	}
	if fenced, err := pairingFenced(ctx, begin, "claim", "host", "device"); fenced || !errors.Is(err, errFenceTest) {
		t.Fatal("failed transaction reported a proven fence")
	}
	if outcome, err := pairingOutcome(ctx, begin, "claim", "device"); outcome.Active || outcome.HostConfirmed || outcome.ClaimID != "" || !errors.Is(err, errFenceTest) {
		t.Fatal("failed transaction reported a committed outcome")
	}
	if confirmed, err := confirmPairingHost(ctx, begin, "claim", "host", "device", "request", time.Time{}); confirmed || !errors.Is(err, errFenceTest) {
		t.Fatal("failed transaction reported Host confirmation")
	}
	if applied, err := applyHostResult(ctx, begin, HostResultUpdate{Status: whalereinv1.OnlineStatus_succeeded}); applied || !errors.Is(err, errFenceTest) {
		t.Fatal("failed transaction reported an applied Host result")
	}
	f := newBoundCommitFixture(t)
	body := append([]byte(nil), f.notice.Body...)
	if accepted, err := f.commit(begin); accepted || !errors.Is(err, errFenceTest) || !bytes.Equal(f.notice.Body, body) {
		t.Fatal("failed transaction reported a committed grant or changed the outbox")
	}
}
