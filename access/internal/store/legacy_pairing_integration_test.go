//go:build integration

package store_test

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"sync"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
	"github.com/whalerein/whalerein/access/internal/store/storetest"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

func committedPairingFixture(t *testing.T) (*store.Store, store.PairingOperation, *whalereinv1.HostRequest, time.Time) {
	t.Helper()
	s, _ := storetest.New(t)
	ctx := context.Background()
	now := time.Now().UTC()
	if _, err := s.EnsureHost(ctx, "legacy-host"); err != nil {
		t.Fatal(err)
	}
	hash := bytes.Repeat([]byte{7}, 32)
	if err := s.CreatePairingToken(ctx, hash, "legacy-host", now.Add(time.Hour), now); err != nil {
		t.Fatal(err)
	}
	op, notice := fixturePairingOperation(t, s, hash, "legacy-host", "legacy-device", "legacy-claim", now, nil)
	var response whalereinv1.PairingClaimResponse
	if err := proto.Unmarshal(op.FinishResponse, &response); err != nil {
		t.Fatal(err)
	}
	response.ProtocolVersion = 1
	op.FinishResponse, _ = proto.Marshal(&response)
	fingerprint := sha256.Sum256(response.DevicePublicKey)
	if used, err := s.CommitPairing(ctx, hash, op.HostID, op.DeviceID, hex.EncodeToString(fingerprint[:]), bytes.Repeat([]byte{8}, 32), now, notice, &op); err != nil || !used {
		t.Fatal("fixture commit failed")
	}
	return s, op, notice, now
}

// A legacy completed receipt can persist an authenticated Host confirmation before its operation is cleaned.
func TestLegacyPairingConfirmationBindsStoredFinish(t *testing.T) {
	s, op, notice, now := committedPairingFixture(t)
	ctx := context.Background()
	if _, err := s.Pool().Exec(ctx, `UPDATE pairing_outcomes SET device_public_key=NULL,protocol_version=NULL,commit_binding_verified=false WHERE claim_id=$1`, op.ClaimID); err != nil {
		t.Fatal(err)
	}
	if confirmed, err := s.ConfirmPairingHost(ctx, op.ClaimID, op.HostID, op.DeviceID, notice.RequestId, "", now); err != nil || !confirmed {
		t.Fatal("valid legacy Host confirmation could not bind original FINISH")
	}
	outcome, err := s.PairingOutcome(ctx, op.ClaimID, op.DeviceID)
	if err != nil || !outcome.HostConfirmed || len(outcome.DevicePublicKey) != 32 || outcome.ProtocolVersion != 1 {
		t.Fatal("legacy confirmation not retained")
	}
}

// A late validated Host acknowledgement remains recoverable after operation and invitation cleanup without a client retry.
func TestLatePairingHostAcknowledgementSurvivesCleanup(t *testing.T) {
	s, op, notice, now := committedPairingFixture(t)
	ctx := context.Background()
	body, _ := proto.Marshal(&whalereinv1.PairingClaimResult{Authorized: true, DeviceId: op.DeviceID})
	applied, err := s.ApplyHostResult(ctx, store.HostResultUpdate{RequestID: notice.RequestId, HostID: op.HostID, DeviceID: op.DeviceID, Status: whalereinv1.OnlineStatus_succeeded, ResultBody: body, At: now})
	if err != nil || !applied {
		t.Fatal("late acknowledgement not accepted")
	}
	if _, err = s.DeleteExpiredPairingTokens(ctx, now.Add(2*time.Hour)); err != nil {
		t.Fatal(err)
	}
	if _, err = s.ClearExpiredBodies(ctx, now.Add(2*time.Hour)); err != nil {
		t.Fatal(err)
	}
	outcome, err := s.PairingOutcome(ctx, op.ClaimID, op.DeviceID)
	if err != nil || !outcome.Active || !outcome.HostConfirmed {
		t.Fatal("late Host confirmation did not survive cleanup")
	}
}

// Late pairing acknowledgements require matching identity and cannot establish an active grant after revocation.
func TestLatePairingAcknowledgementRequiresBindingAndActiveGrant(t *testing.T) {
	for _, variant := range []string{"wrong-host", "wrong-device", "wrong-request", "failed-status", "wrong-body-device", "malformed", "expired", "revoked-first", "confirmed-then-revoked"} {
		t.Run(variant, func(t *testing.T) {
			s, op, notice, now := committedPairingFixture(t)
			ctx := context.Background()
			if variant == "revoked-first" {
				if err := s.RevokeGrant(ctx, op.HostID, op.DeviceID, now); err != nil {
					t.Fatal(err)
				}
			}
			body, _ := proto.Marshal(&whalereinv1.PairingClaimResult{Authorized: true, DeviceId: op.DeviceID})
			update := store.HostResultUpdate{RequestID: notice.RequestId, HostID: op.HostID, DeviceID: op.DeviceID, Status: whalereinv1.OnlineStatus_succeeded, ResultBody: body, At: now}
			switch variant {
			case "wrong-host":
				update.HostID = "other"
			case "wrong-device":
				update.DeviceID = "other"
			case "wrong-request":
				update.RequestID = "other"
			case "failed-status":
				update.Status = whalereinv1.OnlineStatus_failed
			case "wrong-body-device":
				update.ResultBody, _ = proto.Marshal(&whalereinv1.PairingClaimResult{Authorized: true, DeviceId: "other"})
			case "malformed":
				update.ResultBody = []byte{0xff}
			case "expired":
				update.At = time.UnixMilli(notice.ExpiresAtUnixMs).Add(time.Millisecond)
			}
			if _, err := s.ApplyHostResult(ctx, update); err != nil {
				t.Fatal(err)
			}
			if variant == "confirmed-then-revoked" {
				if err := s.RevokeGrant(ctx, op.HostID, op.DeviceID, now); err != nil {
					t.Fatal(err)
				}
			}
			outcome, err := s.PairingOutcome(ctx, op.ClaimID, op.DeviceID)
			if err != nil {
				t.Fatal(err)
			}
			if variant == "confirmed-then-revoked" {
				if !outcome.HostConfirmed || outcome.Active {
					t.Fatal("revoked outcome state incorrect")
				}
			} else if outcome.HostConfirmed {
				t.Fatal("unproven acknowledgement recorded")
			}
		})
	}
}

// Legacy confirmation must reject an original response whose public key does not match the pinned device.
func TestLegacyPairingConfirmationRejectsWrongDeviceKey(t *testing.T) {
	s, op, notice, now := committedPairingFixture(t)
	ctx := context.Background()
	var response whalereinv1.PairingClaimResponse
	if err := proto.Unmarshal(op.FinishResponse, &response); err != nil {
		t.Fatal(err)
	}
	response.DevicePublicKey = bytes.Repeat([]byte{0x5a}, 32)
	wrong, _ := proto.Marshal(&response)
	if _, err := s.Pool().Exec(ctx, `UPDATE pairing_operations SET finish_response=$2 WHERE claim_id=$1`, op.ClaimID, wrong); err != nil {
		t.Fatal(err)
	}
	if _, err := s.Pool().Exec(ctx, `UPDATE pairing_outcomes SET device_public_key=NULL,protocol_version=NULL,commit_binding_verified=false WHERE claim_id=$1`, op.ClaimID); err != nil {
		t.Fatal(err)
	}
	if confirmed, err := s.ConfirmPairingHost(ctx, op.ClaimID, op.HostID, op.DeviceID, notice.RequestId, "", now); err == nil || confirmed {
		t.Fatal("wrong legacy device key was confirmed")
	}
	outcome, err := s.PairingOutcome(ctx, op.ClaimID, op.DeviceID)
	if err != nil || outcome.HostConfirmed {
		t.Fatal("wrong key established durable confirmation")
	}
}

// A synchronous Host acknowledgement received after its COMMIT deadline cannot establish confirmation.
func TestPairingSynchronousExpiredAcknowledgementCannotConfirm(t *testing.T) {
	s, op, notice, now := committedPairingFixture(t)
	ctx := context.Background()
	if _, err := s.Pool().Exec(ctx, `UPDATE requests SET expires_at=$2 WHERE request_id=$1`, notice.RequestId, now.Add(-time.Millisecond)); err != nil {
		t.Fatal(err)
	}
	if confirmed, err := s.ConfirmPairingHost(ctx, op.ClaimID, op.HostID, op.DeviceID, notice.RequestId, "", now); err == nil || confirmed {
		t.Fatal("expired synchronous acknowledgement confirmed")
	}
	outcome, err := s.PairingOutcome(ctx, op.ClaimID, op.DeviceID)
	if err != nil || outcome.HostConfirmed {
		t.Fatal("expired acknowledgement recorded")
	}
}

// Direct and late Host acknowledgements share the exact exclusive COMMIT deadline.
func TestPairingAcknowledgementDeadlineBoundaries(t *testing.T) {
	for _, path := range []string{"direct", "late"} {
		for _, delta := range []time.Duration{-time.Millisecond, 0, time.Millisecond} {
			t.Run(path+delta.String(), func(t *testing.T) {
				s, op, notice, now := committedPairingFixture(t)
				ctx := context.Background()
				deadline := now.Add(30 * time.Second)
				if _, err := s.Pool().Exec(ctx, `UPDATE requests SET expires_at=$2 WHERE request_id=$1`, notice.RequestId, deadline); err != nil {
					t.Fatal(err)
				}
				at := deadline.Add(delta)
				if path == "direct" {
					confirmed, err := s.ConfirmPairingHost(ctx, op.ClaimID, op.HostID, op.DeviceID, notice.RequestId, "", at)
					if delta < 0 {
						if err != nil || !confirmed {
							t.Fatal("valid direct acknowledgement rejected")
						}
					} else if err == nil || confirmed {
						t.Fatal("expired direct acknowledgement accepted")
					}
				} else {
					body, _ := proto.Marshal(&whalereinv1.PairingClaimResult{Authorized: true, DeviceId: op.DeviceID})
					if _, err := s.ApplyHostResult(ctx, store.HostResultUpdate{RequestID: notice.RequestId, HostID: op.HostID, DeviceID: op.DeviceID, Status: whalereinv1.OnlineStatus_succeeded, ResultBody: body, At: at}); err != nil {
						t.Fatal(err)
					}
				}
				outcome, err := s.PairingOutcome(ctx, op.ClaimID, op.DeviceID)
				if err != nil {
					t.Fatal(err)
				}
				if outcome.HostConfirmed != (delta < 0) {
					t.Fatal("deadline produced incorrect confirmation")
				}
			})
		}
	}
}

// An expired legacy pairing with a previously stored valid Host acknowledgement remains recoverable after retention cleanup.
func TestLegacyExpiredPairingReconcilesStoredHostAcknowledgementBeforeCleanup(t *testing.T) {
	s, op, notice, now := committedPairingFixture(t)
	ctx := context.Background()
	body, _ := proto.Marshal(&whalereinv1.PairingClaimResult{Authorized: true, DeviceId: op.DeviceID})
	if _, err := s.Pool().Exec(ctx, `UPDATE requests SET status='succeeded',result_body=$2,updated_at=$3 WHERE request_id=$1`, notice.RequestId, body, now); err != nil {
		t.Fatal(err)
	}
	if _, err := s.Pool().Exec(ctx, `UPDATE pairing_outcomes SET device_public_key=NULL,protocol_version=NULL,commit_binding_verified=false,host_confirmed=false WHERE claim_id=$1`, op.ClaimID); err != nil {
		t.Fatal(err)
	}
	if _, err := s.DeleteExpiredPairingTokens(ctx, now.Add(2*time.Hour)); err != nil {
		t.Fatal(err)
	}
	outcome, err := s.PairingOutcome(ctx, op.ClaimID, op.DeviceID)
	if err != nil || !outcome.Active || !outcome.HostConfirmed || len(outcome.DevicePublicKey) != 32 {
		t.Fatal("valid legacy acknowledgement lost during cleanup")
	}
}

// Cleanup binds a verifiable legacy public key without inventing Host confirmation; a later valid ACK can then confirm it.
func TestLegacyCleanupPreservesKeyWithoutInventingHostConfirmation(t *testing.T) {
	s, op, notice, now := committedPairingFixture(t)
	ctx := context.Background()
	if _, err := s.Pool().Exec(ctx, `UPDATE pairing_outcomes SET device_public_key=NULL,protocol_version=NULL,commit_binding_verified=false WHERE claim_id=$1`, op.ClaimID); err != nil {
		t.Fatal(err)
	}
	if _, err := s.DeleteExpiredPairingTokens(ctx, now.Add(2*time.Hour)); err != nil {
		t.Fatal(err)
	}
	outcome, err := s.PairingOutcome(ctx, op.ClaimID, op.DeviceID)
	if err != nil || !outcome.Active || outcome.HostConfirmed || len(outcome.DevicePublicKey) != 32 {
		t.Fatal("cleanup invented confirmation or lost verified public key")
	}
	body, _ := proto.Marshal(&whalereinv1.PairingClaimResult{Authorized: true, DeviceId: op.DeviceID})
	applied, err := s.ApplyHostResult(ctx, store.HostResultUpdate{RequestID: notice.RequestId, HostID: op.HostID, DeviceID: op.DeviceID, Status: whalereinv1.OnlineStatus_succeeded, ResultBody: body, At: now})
	if err != nil || !applied {
		t.Fatal("valid delayed Host acknowledgement rejected")
	}
	outcome, err = s.PairingOutcome(ctx, op.ClaimID, op.DeviceID)
	if err != nil || !outcome.HostConfirmed {
		t.Fatal("delayed Host acknowledgement was not retained")
	}
}

// Cleanup reconciles only pairing evidence threatened by its own retention cutoff.
func TestLegacyPairingReconciliationSkipsEvidenceOutsideCutoff(t *testing.T) {
	for _, path := range []string{"bodies", "tokens"} {
		t.Run(path, func(t *testing.T) {
			s, op, _, now := committedPairingFixture(t)
			ctx := context.Background()
			if _, err := s.Pool().Exec(ctx, `UPDATE pairing_outcomes SET device_public_key=NULL,protocol_version=NULL,commit_binding_verified=false WHERE claim_id=$1`, op.ClaimID); err != nil {
				t.Fatal(err)
			}
			if path == "bodies" {
				if _, err := s.ClearExpiredBodies(ctx, now); err != nil {
					t.Fatal(err)
				}
			} else {
				if _, err := s.DeleteExpiredPairingTokens(ctx, now); err != nil {
					t.Fatal(err)
				}
			}
			// Inspect stored values directly: PairingOutcome may legitimately
			// prebind retained evidence when explicitly requested by a caller.
			var key []byte
			var confirmed, proof bool
			if err := s.Pool().QueryRow(ctx, `SELECT device_public_key,host_confirmed,commit_binding_verified FROM pairing_outcomes WHERE claim_id=$1`, op.ClaimID).Scan(&key, &confirmed, &proof); err != nil {
				t.Fatal(err)
			}
			if len(key) != 0 || confirmed || proof {
				t.Fatal("cleanup processed evidence outside cutoff")
			}
		})
	}
}

// Concurrent late ACK, both cleanup paths and revocation cannot revive a grant or invent an identity.
func TestConcurrentPairingCleanupAcknowledgementAndRevocation(t *testing.T) {
	for attempt := 0; attempt < 8; attempt++ {
		s, op, notice, now := committedPairingFixture(t)
		ctx := context.Background()
		if _, err := s.Pool().Exec(ctx, `UPDATE pairing_outcomes SET device_public_key=NULL,protocol_version=NULL,commit_binding_verified=false WHERE claim_id=$1`, op.ClaimID); err != nil {
			t.Fatal(err)
		}
		body, _ := proto.Marshal(&whalereinv1.PairingClaimResult{Authorized: true, DeviceId: op.DeviceID})
		start := make(chan struct{})
		type result struct {
			name string
			err  error
		}
		errs := make(chan result, 4)
		var group sync.WaitGroup
		tasks := []struct {
			name string
			run  func() error
		}{
			{"ack", func() error {
				_, err := s.ApplyHostResult(ctx, store.HostResultUpdate{RequestID: notice.RequestId, HostID: op.HostID, DeviceID: op.DeviceID, Status: whalereinv1.OnlineStatus_succeeded, ResultBody: body, At: now})
				return err
			}},
			{"bodies", func() error { _, err := s.ClearExpiredBodies(ctx, now.Add(2*time.Hour)); return err }},
			{"tokens", func() error { _, err := s.DeleteExpiredPairingTokens(ctx, now.Add(2*time.Hour)); return err }},
			{"revoke", func() error { return s.RevokeGrant(ctx, op.HostID, op.DeviceID, now) }},
		}
		for _, task := range tasks {
			group.Add(1)
			go func(item struct {
				name string
				run  func() error
			}) {
				defer group.Done()
				<-start
				errs <- result{item.name, item.run()}
			}(task)
		}
		close(start)
		group.Wait()
		close(errs)
		ackRejected := false
		for result := range errs {
			if result.err != nil {
				// Cleanup may remove the only COMMIT body before this ACK is
				// applied. The explicit conflict must not become confirmation.
				if result.name == "ack" && errors.Is(result.err, store.ErrConflict) {
					ackRejected = true
					continue
				}
				switch result.name {
				case "ack":
					t.Fatal("concurrent acknowledgement failed")
				case "bodies":
					t.Fatal("concurrent body cleanup failed")
				case "tokens":
					t.Fatal("concurrent token cleanup failed")
				case "revoke":
					t.Fatal("concurrent revocation failed")
				}
			}
		}
		outcome, err := s.PairingOutcome(ctx, op.ClaimID, op.DeviceID)
		if err != nil || outcome.Active || !bytes.Equal(outcome.FinishHash, op.FinishHash) {
			t.Fatal("concurrent cleanup or revoke corrupted outcome")
		}
		if ackRejected {
			var confirmed, verified bool
			if err := s.Pool().QueryRow(ctx, `SELECT host_confirmed,host_confirmation_verified FROM pairing_outcomes WHERE claim_id=$1`, op.ClaimID).Scan(&confirmed, &verified); err != nil || confirmed || verified {
				t.Fatal("rejected acknowledgement became a confirmation")
			}
		}
	}
}

// Every eligible legacy outcome is bound across multiple internal cleanup batches without inventing confirmation.
func TestLegacyPairingReconciliationProcessesEveryBatch(t *testing.T) {
	s, op, notice, now := committedPairingFixture(t)
	ctx := context.Background()
	if _, err := s.Pool().Exec(ctx, `UPDATE pairing_outcomes SET device_public_key=NULL,protocol_version=NULL,commit_binding_verified=false WHERE claim_id=$1`, op.ClaimID); err != nil {
		t.Fatal(err)
	}
	var generation int64
	var auth []byte
	if err := s.Pool().QueryRow(ctx, `SELECT grant_generation,auth_hash FROM pairing_outcomes WHERE claim_id=$1`, op.ClaimID).Scan(&generation, &auth); err != nil {
		t.Fatal(err)
	}
	for i := 0; i < 260; i++ {
		claimID := fmt.Sprintf("batch-claim-%04d", i)
		requestID := fmt.Sprintf("batch-request-%04d", i)
		token := sha256.Sum256([]byte(claimID))
		finish := sha256.Sum256([]byte(requestID))
		if err := s.CreatePairingToken(ctx, token[:], op.HostID, now.Add(time.Hour), now); err != nil {
			t.Fatal(err)
		}
		body, err := proto.Marshal(&whalereinv1.PairingClaim{ClaimId: claimID, DeviceId: op.DeviceID, Phase: whalereinv1.PairingPhase_PAIRING_PHASE_COMMIT, DevicePublicKey: bytes.Repeat([]byte{3}, 32), GrantGeneration: uint64(generation), ExpiresAtUnixMs: now.Add(time.Hour).UnixMilli()})
		if err != nil {
			t.Fatal(err)
		}
		_, err = s.Pool().Exec(ctx, `INSERT INTO requests(request_id,host_id,device_id,kind,protocol_version,status,request_body,expires_at,trace_id,forward_after,created_at,updated_at)
          VALUES($1,$2,$3,$4,1,'received',$7,$5,'fixture',$6,$6,$6)`, requestID, op.HostID, op.DeviceID, int32(whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE), now.Add(time.Hour), now, body)
		if err != nil {
			t.Fatal(err)
		}
		response, _ := proto.Marshal(&whalereinv1.PairingClaimResponse{ClaimId: claimID, HostId: op.HostID, DeviceId: op.DeviceID, Phase: whalereinv1.PairingPhase_PAIRING_PHASE_FINISH, DevicePublicKey: bytes.Repeat([]byte{3}, 32), ProtocolVersion: 1})
		_, err = s.Pool().Exec(ctx, `INSERT INTO pairing_operations(claim_id,token_hash,host_id,device_id,hello_hash,hello_response,expires_at,finish_hash,finish_response,commit_request_id,grant_generation,auth_hash,device_generation,host_generation)
          VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,0,0)`, claimID, token[:], op.HostID, op.DeviceID, []byte("h"), []byte("r"), now.Add(time.Hour), finish[:], response, requestID, generation, auth)
		if err != nil {
			t.Fatal(err)
		}
		_, err = s.Pool().Exec(ctx, `INSERT INTO pairing_outcomes(claim_id,token_hash,host_id,device_id,finish_hash,grant_generation,auth_hash,commit_request_id)
          VALUES($1,$2,$3,$4,$5,$6,$7,$8)`, claimID, token[:], op.HostID, op.DeviceID, finish[:], generation, auth, requestID)
		if err != nil {
			t.Fatal(err)
		}
	}
	if _, err := s.DeleteExpiredPairingTokens(ctx, now.Add(2*time.Hour)); err != nil {
		t.Fatal(err)
	}
	var bound, confirmed int
	if err := s.Pool().QueryRow(ctx, `SELECT count(*) FILTER(WHERE length(device_public_key)=32),count(*) FILTER(WHERE host_confirmed) FROM pairing_outcomes WHERE device_id=$1`, op.DeviceID).Scan(&bound, &confirmed); err != nil {
		t.Fatal(err)
	}
	if bound != 261 || confirmed != 0 {
		t.Fatal("cleanup did not process every eligible batch safely")
	}
	_ = notice
}
