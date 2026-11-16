package store

import (
	"context"
	"testing"
	"time"

	"github.com/jackc/pgx/v5"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

// The invitation consumption is a compare-and-set on `pairing_tokens.used_at`.
// A second commit for the same invitation must report `used=false` and must not
// touch device, credential, grant, request or outcome rows.
func TestCommitPairingSecondUseLeavesNoGrantOrCredential(t *testing.T) {
	fixture := newBoundCommitFixture(t)
	responseBody, err := proto.Marshal(fixture.response)
	if err != nil {
		t.Fatal(err)
	}
	claimBody, err := proto.Marshal(fixture.claim)
	if err != nil {
		t.Fatal(err)
	}
	notice := &whalereinv1.HostRequest{
		RequestId:       "request",
		HostId:          "host",
		DeviceId:        "device",
		Kind:            whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE,
		Body:            append([]byte(nil), claimBody...),
		ProtocolVersion: 1,
		ExpiresAtUnixMs: fixture.now.Add(time.Minute).UnixMilli(),
	}
	operation := &PairingOperation{ClaimID: "claim", FinishHash: make([]byte, 32), FinishResponse: responseBody}
	tx := &commitPairingTx{t: t, device: 2, host: 3, tokenUsed: true}
	accepted, err := commitPairing(context.Background(), func(context.Context) (pgx.Tx, error) { return tx, nil },
		fixture.tokenHash, "host", "device", fixture.fingerprint, fixture.authHash, fixture.now, notice, operation)
	if err != nil {
		t.Fatalf("second use error = %v, want nil", err)
	}
	if accepted {
		t.Fatal("second use consumed the invitation")
	}
	forbidden := map[string]bool{
		"device-write":       true,
		"credential-cleanup": true,
		"credential-write":   true,
		"grant-write":        true,
		"notice-write":       true,
		"operation-write":    true,
		"outcome-write":      true,
		"commit":             true,
	}
	for _, step := range tx.steps {
		if forbidden[step] {
			t.Fatalf("second use performed %q: steps=%v", step, tx.steps)
		}
	}
	if len(tx.steps) == 0 || tx.steps[len(tx.steps)-1] != "rollback" {
		t.Fatalf("second use did not roll back: %v", tx.steps)
	}
}
