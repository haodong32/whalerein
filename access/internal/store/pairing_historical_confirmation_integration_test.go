//go:build integration

package store_test

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
	"github.com/whalerein/whalerein/access/internal/store/storetest"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

// Existing confirmation cannot prove Host activation without a bound COMMIT.
func TestLegacyPairingConfirmationRequiresPersistedCommitEvidence(t *testing.T) {
	for _, tc := range []struct {
		name          string
		body          string
		wantConfirmed bool
	}{
		{"valid commit retained", "valid", true},
		{"authorize retained", "authorize", false},
		{"commit body already cleaned", "missing", false},
		{"wrong grant generation", "generation", false},
		{"wrong device key", "key", false},
		{"claim expires before request", "deadline", false},
		{"wrong request protocol", "protocol", false},
		{"commit retained but acknowledgement missing", "missing-ack", false},
		{"acknowledgement names another device", "wrong-ack", false},
		{"acknowledgement after deadline", "late-ack", false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			s, _ := storetest.New(t)
			ctx := context.Background()
			now := time.Now().UTC()
			if _, err := s.EnsureHost(ctx, "host"); err != nil {
				t.Fatal(err)
			}
			hash, authHash := bytes.Repeat([]byte{4}, 32), bytes.Repeat([]byte{5}, 32)
			if err := s.CreatePairingToken(ctx, hash, "host", now.Add(time.Minute), now); err != nil {
				t.Fatal(err)
			}
			op, notice := fixturePairingOperation(t, s, hash, "host", "device", "claim", now, nil)
			var response whalereinv1.PairingClaimResponse
			if err := proto.Unmarshal(op.FinishResponse, &response); err != nil {
				t.Fatal(err)
			}
			response.ProtocolVersion = 1
			op.FinishResponse, _ = proto.Marshal(&response)
			keyHash := sha256.Sum256(response.DevicePublicKey)
			if accepted, err := s.CommitPairing(ctx, hash, "host", "device", hex.EncodeToString(keyHash[:]), authHash, now, notice, &op); err != nil || !accepted {
				t.Fatal("valid setup did not commit")
			}
			body := append([]byte(nil), notice.Body...)
			if tc.body != "valid" && tc.body != "missing" {
				var claim whalereinv1.PairingClaim
				if err := proto.Unmarshal(body, &claim); err != nil {
					t.Fatal(err)
				}
				switch tc.body {
				case "authorize":
					claim.Phase = whalereinv1.PairingPhase_PAIRING_PHASE_AUTHORIZE
				case "generation":
					claim.GrantGeneration++
				case "key":
					claim.DevicePublicKey[0] ^= 1
				case "deadline":
					claim.ExpiresAtUnixMs--
				}
				body, _ = proto.Marshal(&claim)
			} else if tc.body == "missing" {
				body = []byte{}
			}
			ackDevice := "device"
			if tc.body == "wrong-ack" {
				ackDevice = "other"
			}
			ack, err := proto.Marshal(&whalereinv1.PairingClaimResult{Authorized: true, DeviceId: ackDevice})
			if err != nil {
				t.Fatal(err)
			}
			if tc.body == "missing-ack" {
				ack = []byte{}
			}
			ackAt := now
			if tc.body == "late-ack" {
				ackAt = time.UnixMilli(notice.ExpiresAtUnixMs)
			}
			if _, err := s.Pool().Exec(ctx, `UPDATE requests SET request_body=$2,result_body=$3,status='succeeded',updated_at=$4 WHERE request_id=$1`, notice.RequestId, body, ack, ackAt); err != nil {
				t.Fatal(err)
			}
			if tc.body == "protocol" {
				if _, err := s.Pool().Exec(ctx, `UPDATE requests SET protocol_version=2 WHERE request_id=$1`, notice.RequestId); err != nil {
					t.Fatal(err)
				}
			}
			if _, err := s.Pool().Exec(ctx, `UPDATE pairing_outcomes SET host_confirmed=true,commit_binding_verified=false WHERE claim_id=$1`, "claim"); err != nil {
				t.Fatal(err)
			}
			outcome, err := s.PairingOutcome(ctx, "claim", "device")
			if err != nil || !outcome.Active || outcome.HostConfirmed != tc.wantConfirmed {
				t.Fatal("historical Host confirmation is not supported by retained COMMIT evidence")
			}
			if _, err := s.ActiveDeviceByAuthHash(ctx, authHash); err != nil {
				t.Fatal("recovery observation removed the original credential")
			}
			var stored bool
			if err := s.Pool().QueryRow(ctx, `SELECT host_confirmed FROM pairing_outcomes WHERE claim_id=$1`, "claim").Scan(&stored); err != nil || !stored {
				t.Fatal("recovery observation rewrote historical data")
			}
			var retainedBody []byte
			if err := s.Pool().QueryRow(ctx, `SELECT request_body FROM requests WHERE request_id=$1`, notice.RequestId).Scan(&retainedBody); err != nil || !bytes.Equal(retainedBody, body) {
				t.Fatal("recovery observation altered retained outbox evidence")
			}
		})
	}
}

// A new Host acknowledgement cannot confirm an AUTHORIZE-only outbox record.
func TestNewPairingConfirmationRejectsWrongPersistedPhase(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	now := time.Now().UTC()
	if _, err := s.EnsureHost(ctx, "host"); err != nil {
		t.Fatal(err)
	}
	hash, authHash := bytes.Repeat([]byte{4}, 32), bytes.Repeat([]byte{5}, 32)
	if err := s.CreatePairingToken(ctx, hash, "host", now.Add(time.Minute), now); err != nil {
		t.Fatal(err)
	}
	op, notice := fixturePairingOperation(t, s, hash, "host", "device", "claim", now, nil)
	var response whalereinv1.PairingClaimResponse
	if err := proto.Unmarshal(op.FinishResponse, &response); err != nil {
		t.Fatal(err)
	}
	response.ProtocolVersion = 1
	op.FinishResponse, _ = proto.Marshal(&response)
	keyHash := sha256.Sum256(response.DevicePublicKey)
	if accepted, err := s.CommitPairing(ctx, hash, "host", "device", hex.EncodeToString(keyHash[:]), authHash, now, notice, &op); err != nil || !accepted {
		t.Fatal("valid setup did not commit")
	}
	var claim whalereinv1.PairingClaim
	if err := proto.Unmarshal(notice.Body, &claim); err != nil {
		t.Fatal(err)
	}
	claim.Phase = whalereinv1.PairingPhase_PAIRING_PHASE_AUTHORIZE
	body, _ := proto.Marshal(&claim)
	if _, err := s.Pool().Exec(ctx, `UPDATE requests SET request_body=$2 WHERE request_id=$1`, notice.RequestId, body); err != nil {
		t.Fatal(err)
	}
	ack, _ := proto.Marshal(&whalereinv1.PairingClaimResult{Authorized: true, DeviceId: "device"})
	_, _ = s.ApplyHostResult(ctx, store.HostResultUpdate{RequestID: notice.RequestId, HostID: "host", DeviceID: "device", Status: whalereinv1.OnlineStatus_succeeded, ResultBody: ack, At: now})
	outcome, err := s.PairingOutcome(ctx, "claim", "device")
	if err != nil || !outcome.Active || outcome.HostConfirmed {
		t.Fatal("AUTHORIZE-only outbox was accepted as Host COMMIT confirmation")
	}
	if _, err := s.ActiveDeviceByAuthHash(ctx, authHash); err != nil {
		t.Fatal("failed confirmation removed the original credential")
	}
}

// A timely Host ACK may rely on durable COMMIT binding after body cleanup, but
// never on an absent proof or a contradictory retained body.
func TestLatePairingAcknowledgementRequiresVerifiedCleanedCommit(t *testing.T) {
	for _, tc := range []struct {
		name, body                                    string
		keepProof, wantConfirmed, late, wrongProtocol bool
	}{
		{"verified commit body cleaned", "empty", true, true, false, false},
		{"unverified commit body cleaned", "empty", false, false, false, false},
		{"verified flag but contradictory body", "wrong", true, false, false, false},
		{"verified body cleaned but ACK late", "empty", true, false, true, false},
		{"verified body cleaned but protocol changed", "empty", true, false, false, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			s, op, notice, now := committedPairingFixture(t)
			ctx := context.Background()
			body := []byte{}
			if tc.body == "wrong" {
				var claim whalereinv1.PairingClaim
				if err := proto.Unmarshal(notice.Body, &claim); err != nil {
					t.Fatal(err)
				}
				claim.Phase = whalereinv1.PairingPhase_PAIRING_PHASE_AUTHORIZE
				body, _ = proto.Marshal(&claim)
			}
			if _, err := s.Pool().Exec(ctx, `UPDATE requests SET request_body=$2,bodies_cleared=$3 WHERE request_id=$1`, notice.RequestId, body, tc.body == "empty"); err != nil {
				t.Fatal(err)
			}
			if !tc.keepProof {
				if _, err := s.Pool().Exec(ctx, `UPDATE pairing_outcomes SET commit_binding_verified=false WHERE claim_id=$1`, op.ClaimID); err != nil {
					t.Fatal(err)
				}
			}
			if tc.wrongProtocol {
				if _, err := s.Pool().Exec(ctx, `UPDATE requests SET protocol_version=2 WHERE request_id=$1`, notice.RequestId); err != nil {
					t.Fatal(err)
				}
			}
			ack, _ := proto.Marshal(&whalereinv1.PairingClaimResult{Authorized: true, DeviceId: op.DeviceID})
			ackAt := now
			if tc.late {
				ackAt = time.UnixMilli(notice.ExpiresAtUnixMs)
			}
			applied, err := s.ApplyHostResult(ctx, store.HostResultUpdate{RequestID: notice.RequestId, HostID: op.HostID, DeviceID: op.DeviceID, Status: whalereinv1.OnlineStatus_succeeded, ResultBody: ack, At: ackAt})
			if tc.late {
				if err != nil && !errors.Is(err, store.ErrConflict) {
					t.Fatal("late acknowledgement failed unexpectedly")
				}
				stored, getErr := s.GetRequest(ctx, notice.RequestId)
				if getErr != nil || len(stored.ResultBody) != 0 {
					t.Fatal("late acknowledgement restored an expired result body")
				}
			} else if tc.wantConfirmed {
				if err != nil || !applied {
					t.Fatal("timely acknowledgement with durable proof was not applied")
				}
			} else if applied || !errors.Is(err, store.ErrConflict) {
				t.Fatal("unproven acknowledgement was not rejected")
			}
			out, err := s.PairingOutcome(ctx, op.ClaimID, op.DeviceID)
			if err != nil || !out.Active || out.HostConfirmed != tc.wantConfirmed {
				t.Fatal("acknowledgement changed confirmation incorrectly")
			}
			if _, err := s.ActiveDeviceByAuthHash(ctx, bytes.Repeat([]byte{8}, 32)); err != nil {
				t.Fatal("acknowledgement changed the active credential")
			}
		})
	}
}

// Retention must preserve verifiable legacy COMMIT evidence in either order.
func TestLegacyCommitProofSurvivesBothCleanupOrders(t *testing.T) {
	for _, tc := range []struct {
		name                                             string
		operationFirst, operationAlreadyGone, missingKey bool
	}{
		{"operation cleanup first", true, false, false},
		{"request cleanup first", false, false, false},
		{"operation already absent", false, true, false},
		{"operation absent and key missing", false, true, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			s, _ := storetest.New(t)
			ctx := context.Background()
			now := time.Now().UTC()
			if _, err := s.EnsureHost(ctx, "host"); err != nil {
				t.Fatal(err)
			}
			hash, authHash := bytes.Repeat([]byte{4}, 32), bytes.Repeat([]byte{5}, 32)
			if err := s.CreatePairingToken(ctx, hash, "host", now.Add(time.Minute), now); err != nil {
				t.Fatal(err)
			}
			op, notice := fixturePairingOperation(t, s, hash, "host", "device", "claim", now, nil)
			keyHash := sha256.Sum256(bytes.Repeat([]byte{3}, 32))
			if accepted, err := s.CommitPairing(ctx, hash, "host", "device", hex.EncodeToString(keyHash[:]), authHash, now, notice, &op); err != nil || !accepted {
				t.Fatal("valid setup did not commit")
			}
			ack, err := proto.Marshal(&whalereinv1.PairingClaimResult{Authorized: true, DeviceId: "device"})
			if err != nil {
				t.Fatal(err)
			}
			if _, err := s.Pool().Exec(ctx, `UPDATE requests SET result_body=$2,status='succeeded',updated_at=$3 WHERE request_id=$1`, notice.RequestId, ack, now); err != nil {
				t.Fatal(err)
			}
			if _, err := s.Pool().Exec(ctx, `UPDATE pairing_outcomes SET host_confirmed=true,commit_binding_verified=false WHERE claim_id=$1`, "claim"); err != nil {
				t.Fatal(err)
			}
			if tc.missingKey {
				if _, err := s.Pool().Exec(ctx, `UPDATE pairing_outcomes SET device_public_key=NULL,protocol_version=NULL WHERE claim_id=$1`, "claim"); err != nil {
					t.Fatal(err)
				}
			}
			if tc.operationAlreadyGone {
				if _, err := s.Pool().Exec(ctx, `DELETE FROM pairing_operations WHERE claim_id=$1`, "claim"); err != nil {
					t.Fatal(err)
				}
			}
			cutoff := now.Add(2 * time.Minute)
			cleanRequests := func() {
				t.Helper()
				if _, err := s.ClearExpiredBodies(ctx, cutoff); err != nil {
					t.Fatal(err)
				}
			}
			cleanTokens := func() {
				t.Helper()
				if _, err := s.DeleteExpiredPairingTokens(ctx, cutoff); err != nil {
					t.Fatal(err)
				}
			}
			if tc.operationFirst {
				cleanTokens()
				cleanRequests()
			} else {
				cleanRequests()
				cleanTokens()
			}
			result, err := s.PairingOutcome(ctx, "claim", "device")
			if err != nil || !result.Active || result.HostConfirmed == tc.missingKey || result.CommitBindingUnproven != tc.missingKey {
				t.Fatal("cleanup failed to preserve or conservatively withhold proof")
			}
			if _, err := s.ActiveDeviceByAuthHash(ctx, authHash); err != nil {
				t.Fatal("cleanup removed the original credential")
			}
			var proof bool
			if err := s.Pool().QueryRow(ctx, `SELECT commit_binding_verified FROM pairing_outcomes WHERE claim_id=$1`, "claim").Scan(&proof); err != nil || proof == tc.missingKey {
				t.Fatal("durable proof did not reflect retained evidence")
			}
		})
	}
}

// Schema upgrade preserves legacy data and does not infer unavailable proof.
func TestPairingCommitProofMigrationPreservesLegacyState(t *testing.T) {
	for _, missingBody := range []bool{false, true} {
		t.Run(map[bool]string{false: "retained commit", true: "body already gone"}[missingBody], func(t *testing.T) {
			// Build the real pre-010 schema, seed legacy rows, then exercise the
			// production upgrade path.
			s := openStoreAtMigration(t, "009_pairing_outcomes.sql")
			ctx := context.Background()
			now := time.Now().UTC()
			if _, err := s.EnsureHost(ctx, "host"); err != nil {
				t.Fatal(err)
			}
			publicKey := bytes.Repeat([]byte{3}, 32)
			keyHash := sha256.Sum256(publicKey)
			authHash := bytes.Repeat([]byte{5}, 32)
			if err := s.CreatePairedDevice(ctx, "host", "device", hex.EncodeToString(keyHash[:]), authHash, now); err != nil {
				t.Fatal(err)
			}
			var generation int64
			if err := s.Pool().QueryRow(ctx, `SELECT grant_generation FROM grants WHERE host_id='host' AND device_id='device'`).Scan(&generation); err != nil {
				t.Fatal(err)
			}
			claimBody, err := proto.Marshal(&whalereinv1.PairingClaim{
				ClaimId: "claim", DeviceId: "device", DevicePublicKey: publicKey,
				Phase:           whalereinv1.PairingPhase_PAIRING_PHASE_COMMIT,
				ExpiresAtUnixMs: now.Add(time.Minute).UnixMilli(), GrantGeneration: uint64(generation),
			})
			if err != nil {
				t.Fatal(err)
			}
			ackBody, err := proto.Marshal(&whalereinv1.PairingClaimResult{Authorized: true, DeviceId: "device"})
			if err != nil {
				t.Fatal(err)
			}
			requestBody := claimBody
			if missingBody {
				requestBody = []byte{}
			}
			// Keep the later Cloud-session schema. Restore its trigger after the
			// 010 rewind recreates the Host confirmation columns.
			var activationTrigger string
			if err := s.Pool().QueryRow(ctx, `
				SELECT pg_get_triggerdef(oid)
				FROM pg_trigger
				WHERE tgrelid = 'pairing_outcomes'::regclass
				  AND tgname = 'session_pairing_activation' AND NOT tgisinternal`).Scan(&activationTrigger); err != nil {
				t.Fatal(err)
			}
			// Recreate the 009 fields in this disposable test schema.
			if _, err := s.Pool().Exec(ctx, `DROP TRIGGER session_pairing_activation ON pairing_outcomes;
                DROP VIEW grant_authority;
                ALTER TABLE pairing_outcomes DROP COLUMN commit_binding_verified, DROP COLUMN host_confirmation_verified;
                ALTER TABLE requests DROP COLUMN bodies_cleared;
                DELETE FROM schema_migrations WHERE name IN ('010_pairing_commit_binding.sql','016_external_device_revocations.sql')`); err != nil {
				t.Fatal(err)
			}
			if err := s.Migrate(ctx); err != nil {
				t.Fatal(err)
			}
			if _, err := s.Pool().Exec(ctx, activationTrigger); err != nil {
				t.Fatal(err)
			}
			var legacyUnknown bool
			if err := s.Pool().QueryRow(ctx, `SELECT bodies_cleared IS NULL FROM requests WHERE request_id=$1`, legacyEmptyID).Scan(&legacyUnknown); err != nil || !legacyUnknown {
				t.Fatal("upgrade invented retention history for legacy empty request")
			}
			newEmpty := newRequest("new-empty", "host", "device", "new-command", []byte{2}, whalereinv1.OnlineStatus_received, store.ForwardPending, now)
			newEmpty.RequestBody = []byte{}
			if _, err := s.CreateRequest(ctx, newEmpty); err != nil {
				t.Fatal(err)
			}
			var newCleared bool
			if err := s.Pool().QueryRow(ctx, `SELECT bodies_cleared FROM requests WHERE request_id=$1`, newEmpty.RequestID).Scan(&newCleared); err != nil || newCleared {
				t.Fatal("new request did not start with uncleared marker")
			}
			var confirmed, proof, ackProof bool
			if err := s.Pool().QueryRow(ctx, `SELECT host_confirmed,commit_binding_verified,host_confirmation_verified FROM pairing_outcomes WHERE claim_id=$1`, "claim").Scan(&confirmed, &proof, &ackProof); err != nil || !confirmed || proof || ackProof {
				t.Fatal("upgrade changed legacy confirmation or assumed proof")
			}
			if _, err := s.ActiveDeviceByAuthHash(ctx, authHash); err != nil {
				t.Fatal("upgrade removed an active credential")
			}
			outcome, err := s.PairingOutcome(ctx, "claim", "device")
			if err != nil || !outcome.Active || outcome.HostConfirmed == missingBody || outcome.CommitBindingUnproven != missingBody {
				t.Fatal("post-upgrade recovery did not respect retained COMMIT evidence")
			}
			if err := s.Pool().QueryRow(ctx, `SELECT host_confirmed FROM pairing_outcomes WHERE claim_id=$1`, "claim").Scan(&confirmed); err != nil || !confirmed {
				t.Fatal("observation destructively rewrote legacy confirmation")
			}
		})
	}
}

// Legacy ACK upgrade must reject a contradictory retained COMMIT body.
func TestLegacyAckUpgradeChecksRetainedCommitEvenWithPriorBinding(t *testing.T) {
	for _, tc := range []struct {
		name, body                   string
		priorAckProof, wantConfirmed bool
	}{
		{"valid retained COMMIT", "valid", false, true},
		{"cleared with prior ACK proof", "cleared", true, true},
		{"cleared without prior ACK proof", "cleared", false, false},
		{"retained AUTHORIZE", "phase", false, false},
		{"retained wrong key", "key", false, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			s, op, notice, now := committedPairingFixture(t)
			ctx := context.Background()
			body := append([]byte(nil), notice.Body...)
			if tc.body == "phase" || tc.body == "key" {
				var claim whalereinv1.PairingClaim
				if err := proto.Unmarshal(body, &claim); err != nil {
					t.Fatal(err)
				}
				if tc.body == "phase" {
					claim.Phase = whalereinv1.PairingPhase_PAIRING_PHASE_AUTHORIZE
				} else {
					claim.DevicePublicKey[0] ^= 1
				}
				body, _ = proto.Marshal(&claim)
			}
			ack, _ := proto.Marshal(&whalereinv1.PairingClaimResult{Authorized: true, DeviceId: op.DeviceID})
			cleared := tc.body == "cleared"
			if cleared {
				body = []byte{}
				ack = []byte{}
			}
			if _, err := s.Pool().Exec(ctx, `UPDATE requests SET request_body=$2,result_body=$3,bodies_cleared=$4,status='succeeded',updated_at=$5 WHERE request_id=$1`, notice.RequestId, body, ack, cleared, now); err != nil {
				t.Fatal(err)
			}
			if _, err := s.Pool().Exec(ctx, `UPDATE pairing_outcomes SET host_confirmed=true,commit_binding_verified=true,host_confirmation_verified=$2 WHERE claim_id=$1`, op.ClaimID, tc.priorAckProof); err != nil {
				t.Fatal(err)
			}
			outcome, err := s.PairingOutcome(ctx, op.ClaimID, op.DeviceID)
			if err != nil || !outcome.Active || outcome.HostConfirmed != tc.wantConfirmed {
				t.Fatal("outcome confirmation did not match retained evidence")
			}
			var verified bool
			if err := s.Pool().QueryRow(ctx, `SELECT host_confirmation_verified FROM pairing_outcomes WHERE claim_id=$1`, op.ClaimID).Scan(&verified); err != nil || verified != tc.wantConfirmed {
				t.Fatal("ACK proof was upgraded without matching evidence")
			}
			if _, err := s.ActiveDeviceByAuthHash(ctx, bytes.Repeat([]byte{8}, 32)); err != nil {
				t.Fatal("observation removed the original credential")
			}
		})
	}
}

// An unfinished legacy COMMIT can be rebound only from complete evidence.
func TestLegacyPendingCommitPrebindsWithoutInventingHostConfirmation(t *testing.T) {
	for _, tc := range []struct {
		name                         string
		wrongPhase, missingOperation bool
	}{
		{"valid retained operation", false, false},
		{"wrong persisted phase", true, false},
		{"operation missing", false, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			s, op, notice, _ := committedPairingFixture(t)
			ctx := context.Background()
			if _, err := s.Pool().Exec(ctx, `UPDATE pairing_outcomes SET device_public_key=NULL,protocol_version=NULL,commit_binding_verified=false,host_confirmed=false WHERE claim_id=$1`, op.ClaimID); err != nil {
				t.Fatal(err)
			}
			if tc.wrongPhase {
				var claim whalereinv1.PairingClaim
				if err := proto.Unmarshal(notice.Body, &claim); err != nil {
					t.Fatal(err)
				}
				claim.Phase = whalereinv1.PairingPhase_PAIRING_PHASE_AUTHORIZE
				body, _ := proto.Marshal(&claim)
				if _, err := s.Pool().Exec(ctx, `UPDATE requests SET request_body=$2 WHERE request_id=$1`, notice.RequestId, body); err != nil {
					t.Fatal(err)
				}
			}
			if tc.missingOperation {
				if _, err := s.Pool().Exec(ctx, `DELETE FROM pairing_operations WHERE claim_id=$1`, op.ClaimID); err != nil {
					t.Fatal(err)
				}
			}
			outcome, err := s.PairingOutcome(ctx, op.ClaimID, op.DeviceID)
			if err != nil || !outcome.Active || outcome.HostConfirmed {
				t.Fatal("legacy observation lost grant or invented confirmation")
			}
			wantBound := !tc.wrongPhase && !tc.missingOperation
			if outcome.CommitBindingUnproven == wantBound || (len(outcome.DevicePublicKey) == 32) != wantBound {
				t.Fatal("legacy COMMIT evidence was not bound conservatively")
			}
		})
	}
}

// Token retention must reconcile a later-expiring operation before cascading it.
func TestTokenCleanupPreservesLegacyProofWhenOperationExpiresLater(t *testing.T) {
	for _, tc := range []struct {
		name                     string
		confirmed                bool
		ack                      string
		wantProof, wantConfirmed bool
	}{
		{"unconfirmed with no acknowledgement", false, "missing", true, false},
		{"confirmed with valid acknowledgement", true, "valid", true, true},
		{"confirmed with missing acknowledgement", true, "missing", true, false},
		{"confirmed with wrong device acknowledgement", true, "wrong", true, false},
		{"confirmed with late acknowledgement", true, "late", true, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			s, op, notice, now := committedPairingFixture(t)
			ctx := context.Background()
			if _, err := s.Pool().Exec(ctx, `UPDATE pairing_outcomes SET device_public_key=NULL,protocol_version=NULL,host_confirmed=$2,commit_binding_verified=false WHERE claim_id=$1`, op.ClaimID, tc.confirmed); err != nil {
				t.Fatal(err)
			}
			if tc.ack != "missing" {
				device := op.DeviceID
				if tc.ack == "wrong" {
					device = "other"
				}
				ack, err := proto.Marshal(&whalereinv1.PairingClaimResult{Authorized: true, DeviceId: device})
				if err != nil {
					t.Fatal(err)
				}
				received := now
				if tc.ack == "late" {
					received = time.UnixMilli(notice.ExpiresAtUnixMs)
				}
				if _, err := s.Pool().Exec(ctx, `UPDATE requests SET result_body=$2,status='succeeded',updated_at=$3 WHERE request_id=$1`, notice.RequestId, ack, received); err != nil {
					t.Fatal(err)
				}
			}
			if _, err := s.Pool().Exec(ctx, `UPDATE pairing_tokens SET expires_at=$2 WHERE token_hash=$1`, op.TokenHash, now.Add(time.Hour)); err != nil {
				t.Fatal(err)
			}
			if _, err := s.Pool().Exec(ctx, `UPDATE pairing_operations SET expires_at=$2 WHERE claim_id=$1`, op.ClaimID, now.Add(3*time.Hour)); err != nil {
				t.Fatal(err)
			}
			if _, err := s.DeleteExpiredPairingTokens(ctx, now.Add(2*time.Hour)); err != nil {
				t.Fatal(err)
			}
			if _, err := s.PairingOperation(ctx, op.ClaimID); !errors.Is(err, store.ErrNotFound) {
				t.Fatal("expired token did not cascade operation")
			}
			stored, err := s.GetRequest(ctx, notice.RequestId)
			if err != nil || len(stored.RequestBody) == 0 {
				t.Fatal("request evidence unexpectedly missing")
			}
			outcome, err := s.PairingOutcome(ctx, op.ClaimID, op.DeviceID)
			if err != nil || !outcome.Active || outcome.HostConfirmed != tc.wantConfirmed || outcome.CommitBindingUnproven == tc.wantProof || (len(outcome.DevicePublicKey) == 32) != tc.wantProof {
				t.Fatal("token cleanup did not preserve or withhold proof correctly")
			}
		})
	}
}
