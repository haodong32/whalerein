//go:build integration

package store_test

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/base64"
	"errors"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/pairing"
	"github.com/whalerein/whalerein/access/internal/store/storetest"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

type claimReviewPresence struct{}

func (claimReviewPresence) IsHostOnline(context.Context, string) bool { return true }

func (claimReviewPresence) Available() bool { return true }

type claimReviewPolicy struct{}

func (claimReviewPolicy) ApplyPairing(context.Context, string, string) error { return nil }

type claimReviewTransport struct{ calls int }

func (*claimReviewTransport) SupportsPairing(string) bool { return true }
func (t *claimReviewTransport) PublishAndAwait(_ context.Context, host string, request *whalereinv1.HostRequest, _ time.Duration) (*whalereinv1.HostResult, error) {
	t.calls++
	var claim whalereinv1.PairingClaim
	if err := proto.Unmarshal(request.Body, &claim); err != nil {
		return nil, err
	}
	body, err := proto.Marshal(&whalereinv1.PairingClaimResult{Authorized: true, DeviceId: claim.DeviceId})
	if err != nil {
		return nil, err
	}
	return &whalereinv1.HostResult{RequestId: request.RequestId, HostId: host, DeviceId: request.DeviceId,
		Status: whalereinv1.OnlineStatus_succeeded, ResultBody: body}, nil
}

// A retained outbox must not be republished when it contradicts the committed
// proof, even while its original grant and invitation remain recoverable.
func TestClaimDoesNotPublishContradictoryRetainedCommit(t *testing.T) {
	for _, contradictory := range []bool{false, true} {
		name := "valid COMMIT can regain a Host confirmation"
		if contradictory {
			name = "contradictory AUTHORIZE must not be published"
		}
		t.Run(name, func(t *testing.T) {
			s, _ := storetest.New(t)
			ctx := context.Background()
			now := time.Now().UTC()
			if _, err := s.EnsureHost(ctx, "host"); err != nil {
				t.Fatal(err)
			}
			token := base64.RawURLEncoding.EncodeToString(bytes.Repeat([]byte{4}, 32))
			credential := base64.RawURLEncoding.EncodeToString(bytes.Repeat([]byte{8}, 32))
			tokenHash := sha256.Sum256([]byte(token))
			authHash := sha256.Sum256([]byte(credential))
			if err := s.CreatePairingToken(ctx, tokenHash[:], "host", now.Add(time.Minute), now); err != nil {
				t.Fatal(err)
			}
			op, notice := fixturePairingOperation(t, s, tokenHash[:], "host", "device", "claim", now, nil)
			finish := &whalereinv1.PairingClaimRequest{Token: token, ClaimId: "claim", DeviceId: "device",
				Phase: whalereinv1.PairingPhase_PAIRING_PHASE_FINISH, NoiseMessage: []byte("synthetic-noise"),
				Confirmed: true, DeviceToken: credential, ProtocolVersion: 1}
			encoded, err := proto.MarshalOptions{Deterministic: true}.Marshal(finish)
			if err != nil {
				t.Fatal(err)
			}
			digest := sha256.Sum256(encoded)
			op.FinishHash = digest[:]
			if ok, err := s.CommitPairing(ctx, tokenHash[:], "host", "device", fixtureKeyPin(3), authHash[:], now, notice, &op); err != nil || !ok {
				t.Fatal("valid setup commit failed")
			}
			if contradictory {
				var claim whalereinv1.PairingClaim
				if err := proto.Unmarshal(notice.Body, &claim); err != nil {
					t.Fatal(err)
				}
				claim.Phase = whalereinv1.PairingPhase_PAIRING_PHASE_AUTHORIZE
				body, err := proto.Marshal(&claim)
				if err != nil {
					t.Fatal(err)
				}
				if _, err := s.Pool().Exec(ctx, `UPDATE requests SET request_body=$2 WHERE request_id=$1`, notice.RequestId, body); err != nil {
					t.Fatal(err)
				}
			}
			transport := &claimReviewTransport{}
			service, err := pairing.New(pairing.Dependencies{Store: s, Policy: claimReviewPolicy{}, Transport: transport, Presence: claimReviewPresence{},
				Config: pairing.Config{ProtocolVersion: 1, ClaimTTL: time.Minute, ClaimTimeout: time.Second}})
			if err != nil {
				t.Fatal(err)
			}
			result, err := service.Claim(ctx, finish)
			if contradictory {
				if !errors.Is(err, pairing.ErrPairingCommittedUnconfirmed) || result != nil || transport.calls != 0 {
					t.Fatalf("contradictory retained COMMIT was not held: calls=%d", transport.calls)
				}
			} else if err != nil || result == nil || result.DeviceId != "device" || transport.calls != 1 {
				t.Fatalf("valid retained COMMIT did not recover: calls=%d", transport.calls)
			}
			if active, err := s.HasActiveGrant(ctx, "host", "device"); err != nil || !active {
				t.Fatal("observation changed the active grant")
			}
			if _, err := s.ActiveDeviceByAuthHash(ctx, authHash[:]); err != nil {
				t.Fatal("observation changed the credential")
			}
		})
	}
}
