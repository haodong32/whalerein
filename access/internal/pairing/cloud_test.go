package pairing

import (
	"bytes"
	"context"
	"encoding/base64"
	"errors"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

func TestCloudPairingSeparatesInvitationTokensAndRequiresAnExactReplay(t *testing.T) {
	f := newFixture(t)
	ctx := context.Background()
	scope := CloudScope{
		Issuer: "issuer", Audience: "https://regional.fixture.test", RegionID: "region",
		WorkspaceID: "workspace", Revision: "revision", HostID: "h1", DeviceID: "device", OperationID: "operation",
		HostKey: bytes.Repeat([]byte{8}, 32), DeviceKey: bytes.Repeat([]byte{4}, 32), ExpiresAt: f.now.Add(time.Hour),
	}
	hello := &whalereinv1.PairingClaimRequest{ClaimId: scope.OperationID, DeviceId: scope.DeviceID,
		NoiseMessage: []byte("initial-proof"), Phase: whalereinv1.PairingPhase_PAIRING_PHASE_HELLO, ProtocolVersion: 1}
	finish := proto.Clone(hello).(*whalereinv1.PairingClaimRequest)
	finish.Phase = whalereinv1.PairingPhase_PAIRING_PHASE_FINISH
	finish.NoiseMessage = []byte("final-proof")
	finish.Confirmed = true
	finish.DeviceToken = base64.RawURLEncoding.EncodeToString(bytes.Repeat([]byte{3}, 32))
	f.transport.awaitFn = func(req *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
		var claim whalereinv1.PairingClaim
		if err := proto.Unmarshal(req.Body, &claim); err != nil {
			return nil, err
		}
		if !bytes.Equal(claim.DevicePublicKey, scope.DeviceKey) {
			t.Fatal("Access did not relay the approved key")
		}
		if claim.Phase == whalereinv1.PairingPhase_PAIRING_PHASE_HELLO &&
			claim.TokenHash != pairingPrologueHash(scope.tokenHash()) {
			t.Fatal("Noise prologue lost the approved scope")
		}
		result, err := defaultPairingAnswers(req)
		if err != nil {
			return nil, err
		}
		var reply whalereinv1.PairingClaimResult
		if err := proto.Unmarshal(result.ResultBody, &reply); err != nil {
			return nil, err
		}
		reply.HostPublicKey = scope.HostKey
		if claim.GetPhase() == whalereinv1.PairingPhase_PAIRING_PHASE_COMMIT {
			reply.GrantId = "grant_cloud_hostowned"
		}
		result.ResultBody, err = proto.Marshal(&reply)
		return result, err
	}
	rawInvitation := proto.Clone(hello).(*whalereinv1.PairingClaimRequest)
	rawInvitation.Token = pairingPrologueHash(scope.tokenHash())
	if _, err := f.service.ClaimCloud(ctx, rawInvitation, scope); !errors.Is(err, ErrInvalidClaim) {
		t.Fatalf("Cloud accepted an invitation token: %v", err)
	}
	expired := scope
	expired.ExpiresAt = f.now
	if _, err := f.service.ClaimCloud(ctx, hello, expired); !errors.Is(err, ErrTokenExpired) {
		t.Fatalf("Cloud accepted expired scope: %v", err)
	}
	f.store.createErr = errors.New("storage unavailable")
	if _, err := f.service.ClaimCloud(ctx, hello, scope); !errors.Is(err, ErrInternal) {
		t.Fatalf("unexpected persistence error: %v", err)
	}
	f.store.createErr = nil
	if _, err := f.service.ClaimCloud(ctx, hello, scope); err != nil {
		t.Fatal(err)
	}
	if _, err := f.service.ClaimCloud(ctx, hello, scope); err != nil {
		t.Fatal(err)
	}
	if len(f.transport.requests) != 1 {
		t.Fatal("HELLO was sent twice")
	}
	if _, err := f.service.Claim(ctx, rawInvitation); !errors.Is(err, ErrUnauthorized) {
		t.Fatalf("Standalone redeemed a Cloud proof: %v", err)
	}
	conflicting := scope
	conflicting.WorkspaceID = "other-workspace"
	if _, err := f.service.ClaimCloud(ctx, hello, conflicting); !errors.Is(err, store.ErrOperationConflict) {
		t.Fatalf("Cloud accepted contradictory scope: %v", err)
	}
	// Another regional process can consume this exact intent between the
	// initial operation read and the transaction. Keep the claim recoverable.
	used := false
	f.store.useResult = &used
	if _, err := f.service.ClaimCloud(ctx, finish, scope); !errors.Is(err, ErrPairingCommittedUnconfirmed) {
		t.Fatalf("concurrent FINISH lost its pending operation: %v", err)
	}
	f.store.useResult = nil
	f.policy.err = errors.New("policy unavailable")
	if _, err := f.service.ClaimCloud(ctx, finish, scope); !errors.Is(err, ErrHostUnavailable) {
		t.Fatalf("Cloud returned an uninstalled grant: %v", err)
	}
	if _, err := f.service.ClaimCloud(ctx, finish, scope); !errors.Is(err, ErrHostUnavailable) {
		t.Fatalf("Cloud recovery ignored the regional policy error: %v", err)
	}
	f.policy.err = nil
	reply, err := f.service.ClaimCloud(ctx, finish, scope)
	if err != nil || reply.GetDeviceId() != scope.DeviceID || reply.GetClaimId() != scope.OperationID {
		t.Fatalf("Cloud recovery lost original identity: %v", err)
	}
	if reply.GetGrantId() != "grant_cloud_hostowned" || f.store.outcomes[scope.OperationID].GrantID != "grant_cloud_hostowned" {
		t.Fatalf("Cloud response grant = %q", reply.GetGrantId())
	}
	if f.store.confirmCalls != 1 {
		t.Fatalf("Host activation count %d, want 1", f.store.confirmCalls)
	}
}

// A first Cloud pairing attempted while the Host is offline fails before any
// Host contact and establishes no usable grant. The same operation can pair
// once after the Host becomes ready.
func TestCloudPairingWhileHostOfflineCreatesNoUsableGrant(t *testing.T) {
	f := newFixture(t)
	ctx := context.Background()
	scope := CloudScope{
		Issuer: "issuer", Audience: "https://regional.fixture.test", RegionID: "region",
		WorkspaceID: "workspace", Revision: "revision", HostID: "h1", DeviceID: "device", OperationID: "operation",
		HostKey: []byte("host-key"), DeviceKey: bytes.Repeat([]byte{4}, 32), ExpiresAt: f.now.Add(time.Hour),
	}
	hello := &whalereinv1.PairingClaimRequest{ClaimId: scope.OperationID, DeviceId: scope.DeviceID,
		NoiseMessage: []byte("initial-proof"), Phase: whalereinv1.PairingPhase_PAIRING_PHASE_HELLO, ProtocolVersion: 1}
	finish := proto.Clone(hello).(*whalereinv1.PairingClaimRequest)
	finish.Phase = whalereinv1.PairingPhase_PAIRING_PHASE_FINISH
	finish.NoiseMessage = []byte("final-proof")
	finish.Confirmed = true
	finish.DeviceToken = base64.RawURLEncoding.EncodeToString(bytes.Repeat([]byte{3}, 32))

	f.transport.awaitFn = func(req *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
		result, err := defaultPairingAnswers(req)
		if err != nil {
			return nil, err
		}
		var reply whalereinv1.PairingClaimResult
		if err := proto.Unmarshal(result.ResultBody, &reply); err != nil {
			return nil, err
		}
		reply.HostPublicKey = scope.HostKey
		result.ResultBody, err = proto.Marshal(&reply)
		return result, err
	}
	f.presence.online["h1"] = false
	if _, err := f.service.ClaimCloud(ctx, hello, scope); !errors.Is(err, ErrHostOffline) {
		t.Fatalf("offline first pairing = %v", err)
	}
	if len(f.transport.requests) != 0 || f.store.confirmCalls != 0 || len(f.store.grants) != 0 {
		t.Fatalf("offline first pairing reached the Host or created a grant: requests=%d confirms=%d grants=%v",
			len(f.transport.requests), f.store.confirmCalls, f.store.grants)
	}

	f.presence.online["h1"] = true
	if _, err := f.service.ClaimCloud(ctx, hello, scope); err != nil {
		t.Fatalf("recovered first pairing HELLO: %v", err)
	}
	if _, err := f.service.ClaimCloud(ctx, finish, scope); err != nil {
		t.Fatalf("recovered first pairing FINISH: %v", err)
	}
	if len(f.store.grants) != 1 || f.store.confirmCalls != 1 {
		t.Fatalf("recovered pairing grants=%v confirms=%d", f.store.grants, f.store.confirmCalls)
	}
}
