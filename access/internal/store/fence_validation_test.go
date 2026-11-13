package store

import (
	"context"
	"errors"
	"testing"

	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

func TestCommitPairingRejectsInvalidFencedHandshakeBeforeDatabaseAccess(t *testing.T) {
	fixture := newBoundCommitFixture(t)
	response := fixture.response
	claim := fixture.claim
	encode := func(message proto.Message) []byte {
		t.Helper()
		value, err := proto.Marshal(message)
		if err != nil {
			t.Fatal(err)
		}
		return value
	}
	for _, name := range []string{"missing-operation", "missing-notice", "invalid-response", "invalid-claim", "plaintext-token", "short-hash", "wrong-response-claim", "wrong-notice-claim", "wrong-host", "wrong-device", "wrong-phase", "short-key"} {
		t.Run(name, func(t *testing.T) {
			response := proto.Clone(response).(*whalereinv1.PairingClaimResponse)
			claim := proto.Clone(claim).(*whalereinv1.PairingClaim)
			op := &PairingOperation{ClaimID: "claim", FinishHash: make([]byte, 32)}
			notice := proto.Clone(fixture.notice).(*whalereinv1.HostRequest)
			switch name {
			case "plaintext-token":
				response.DeviceToken = "synthetic"
			case "short-hash":
				op.FinishHash = make([]byte, 31)
			case "wrong-response-claim":
				response.ClaimId = "other"
			case "wrong-notice-claim":
				claim.ClaimId = "other"
			case "wrong-host":
				response.HostId = "other"
			case "wrong-device":
				response.DeviceId = "other"
			case "wrong-phase":
				response.Phase = whalereinv1.PairingPhase_PAIRING_PHASE_HELLO
			case "short-key":
				response.DevicePublicKey = make([]byte, 31)
			}
			op.FinishResponse, notice.Body = encode(response), encode(claim)
			switch name {
			case "missing-operation":
				op = nil
			case "missing-notice":
				notice = nil
			case "invalid-response":
				op.FinishResponse = []byte{0xff}
			case "invalid-claim":
				notice.Body = []byte{0xff}
			}
			// No database exists: returning ErrConflict proves rejection before any pool access.
			accepted, err := (&Store{}).CommitPairing(context.Background(), fixture.tokenHash, "host", "device", fixture.fingerprint, fixture.authHash, fixture.now, notice, op)
			if accepted || !errors.Is(err, ErrConflict) {
				t.Fatalf("expected conflict before database access; accepted=%v err=%v", accepted, err)
			}
		})
	}
}
