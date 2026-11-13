package store

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"time"

	"github.com/jackc/pgx/v5"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

// PairingOperation contains only public handshake data and credential hashes.
// FinishResponse never contains a plaintext transport credential.
type PairingOperation struct {
	ClaimID          string
	TokenHash        []byte
	HostID           string
	DeviceID         string
	HelloHash        []byte
	HelloResponse    []byte
	ExpiresAt        time.Time
	FinishHash       []byte
	FinishResponse   []byte
	CommitRequestID  string
	GrantGeneration  uint64
	DeviceGeneration int64
	HostGeneration   int64
	AuthHash         []byte
	Active           bool
}

// DeviceGeneration captures a revocation fence before any asynchronous HELLO work.
func (s *Store) DeviceGeneration(ctx context.Context, deviceID string) (int64, error) {
	var generation int64
	err := s.db.QueryRow(ctx, `SELECT COALESCE((SELECT revocation_generation FROM devices WHERE device_id=$1),0)`, deviceID).Scan(&generation)
	return generation, err
}

// HostGeneration captures the independent Host-grant revocation fence.
func (s *Store) HostGeneration(ctx context.Context, hostID, deviceID string) (int64, error) {
	var generation int64
	err := s.db.QueryRow(ctx, `SELECT COALESCE((SELECT revocation_generation FROM grants WHERE host_id=$1 AND device_id=$2),0)`, hostID, deviceID).Scan(&generation)
	return generation, err
}

// PairingGenerations takes both fencing values in one database snapshot before token-lock waits.
func (s *Store) PairingGenerations(ctx context.Context, tokenHash []byte, deviceID string) (int64, int64, error) {
	var deviceGeneration, hostGeneration int64
	err := s.db.QueryRow(ctx, `SELECT COALESCE((SELECT revocation_generation FROM devices WHERE device_id=$2),0),
       COALESCE((SELECT revocation_generation FROM grants WHERE host_id=t.host_id AND device_id=$2),0)
       FROM pairing_tokens t WHERE t.token_hash=$1`, tokenHash, deviceID).Scan(&deviceGeneration, &hostGeneration)
	return deviceGeneration, hostGeneration, wrapRequestScan(err)
}

// SavePairingOperation atomically compares the pre-HELLO fence and reserves the claim.
func (s *Store) SavePairingOperation(ctx context.Context, op PairingOperation) error {
	return savePairingOperation(ctx, s.poolBegin, op)
}

func savePairingOperation(ctx context.Context, begin func(context.Context) (pgx.Tx, error), op PairingOperation) error {
	tx, err := begin(ctx)
	if err != nil {
		return err
	}
	defer func() { _ = tx.Rollback(ctx) }()
	if _, err = tx.Exec(ctx, `SELECT pg_advisory_xact_lock(hashtextextended('whalerein-device:' || $1, 0))`, op.DeviceID); err != nil {
		return err
	}
	var generation int64
	err = tx.QueryRow(ctx, `SELECT COALESCE((SELECT revocation_generation FROM devices WHERE device_id=$1),0)`, op.DeviceID).Scan(&generation)
	if err != nil {
		return err
	}
	if generation != op.DeviceGeneration {
		return ErrPairingFenced
	}
	var hostGeneration int64
	err = tx.QueryRow(ctx, `SELECT COALESCE((SELECT revocation_generation FROM grants WHERE host_id=$1 AND device_id=$2),0)`, op.HostID, op.DeviceID).Scan(&hostGeneration)
	if err != nil {
		return err
	}
	if hostGeneration != op.HostGeneration {
		return ErrPairingFenced
	}
	var completed bool
	if err = tx.QueryRow(ctx, `SELECT EXISTS(SELECT 1 FROM pairing_outcomes WHERE claim_id=$1)`, op.ClaimID).Scan(&completed); err != nil {
		return err
	}
	if completed {
		return ErrConflict
	}
	_, err = tx.Exec(ctx, `INSERT INTO pairing_operations
        (claim_id,token_hash,host_id,device_id,hello_hash,hello_response,expires_at,device_generation,host_generation)
        VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)`, op.ClaimID, op.TokenHash, op.HostID, op.DeviceID, op.HelloHash, op.HelloResponse, op.ExpiresAt, op.DeviceGeneration, op.HostGeneration)
	if isUniqueViolation(err) {
		return ErrConflict
	}
	if err != nil {
		return err
	}
	// Device-fence contention must not extend the original invitation or
	// pending HELLO lifetime. The transaction rolls back if either expired.
	var databaseNow, tokenExpiry time.Time
	if err = tx.QueryRow(ctx, `SELECT clock_timestamp(),expires_at FROM pairing_tokens WHERE token_hash=$1 AND host_id=$2`, op.TokenHash, op.HostID).Scan(&databaseNow, &tokenExpiry); err != nil {
		return err
	}
	if applicationNow := time.Now().UTC(); databaseNow.Before(applicationNow) {
		databaseNow = applicationNow
	}
	if !databaseNow.Before(tokenExpiry) {
		return ErrPairingInvitationExpired
	}
	if !databaseNow.Before(op.ExpiresAt) {
		return ErrPairingIntentExpired
	}
	return tx.Commit(ctx)
}

// PairingFenced proves an unfinished claim cannot commit.
// The same device lock also serializes FINISH, so a completed claim is never
// classified as safely discardable merely because a later revoke occurred.
func (s *Store) PairingFenced(ctx context.Context, claimID, hostID, deviceID string) (bool, error) {
	return pairingFenced(ctx, s.poolBegin, claimID, hostID, deviceID)
}

func pairingFenced(ctx context.Context, begin func(context.Context) (pgx.Tx, error), claimID, hostID, deviceID string) (bool, error) {
	tx, err := begin(ctx)
	if err != nil {
		return false, err
	}
	defer func() { _ = tx.Rollback(ctx) }()
	if _, err = tx.Exec(ctx, `SELECT pg_advisory_xact_lock(hashtextextended('whalerein-device:' || $1, 0))`, deviceID); err != nil {
		return false, err
	}
	var capturedDevice, capturedHost, currentDevice, currentHost int64
	var finished []byte
	err = tx.QueryRow(ctx, `SELECT device_generation,host_generation,finish_hash FROM pairing_operations WHERE claim_id=$1 AND host_id=$2 AND device_id=$3`, claimID, hostID, deviceID).Scan(&capturedDevice, &capturedHost, &finished)
	if err != nil {
		return false, err
	}
	if len(finished) != 0 {
		return false, nil
	}
	err = tx.QueryRow(ctx, `SELECT COALESCE((SELECT revocation_generation FROM devices WHERE device_id=$1),0),COALESCE((SELECT revocation_generation FROM grants WHERE host_id=$2 AND device_id=$1),0)`, deviceID, hostID).Scan(&currentDevice, &currentHost)
	if err != nil {
		return false, err
	}
	return capturedDevice != currentDevice || capturedHost != currentHost, nil
}

// PairingOperation reads progress and verifies the exact committed grant is live.
func (s *Store) PairingOperation(ctx context.Context, claimID string) (PairingOperation, error) {
	var op PairingOperation
	err := s.db.QueryRow(ctx, `SELECT p.claim_id,p.token_hash,p.host_id,p.device_id,p.hello_hash,p.hello_response,p.expires_at,
        p.finish_hash,p.finish_response,COALESCE(p.commit_request_id,''),COALESCE(p.grant_generation,0),p.auth_hash,
        EXISTS(SELECT 1 FROM grants g JOIN device_credentials c ON c.device_id=g.device_id
            WHERE g.host_id=p.host_id AND g.device_id=p.device_id AND g.revoked_at IS NULL
            AND g.grant_generation=p.grant_generation AND c.auth_hash=p.auth_hash)
        FROM pairing_operations p WHERE claim_id=$1`, claimID).Scan(&op.ClaimID, &op.TokenHash, &op.HostID, &op.DeviceID,
		&op.HelloHash, &op.HelloResponse, &op.ExpiresAt, &op.FinishHash, &op.FinishResponse, &op.CommitRequestID,
		&op.GrantGeneration, &op.AuthHash, &op.Active)
	return op, wrapRequestScan(err)
}

// PairingOutcome is minimal committed FINISH evidence retained independently of invitations.
type PairingOutcome struct {
	ClaimID, HostID, DeviceID string
	TokenHash, FinishHash     []byte
	DevicePublicKey           []byte
	ProtocolVersion           uint32
	HostConfirmed             bool
	Active                    bool
	// Old retained state with no verifiable COMMIT must remain unconfirmed.
	CommitBindingUnproven bool
}

// verifyPersistedCommitTx checks the actual durable outbox under the caller's
// device fence. Missing, cleaned or malformed legacy evidence is not proof.
func verifyPersistedCommitTx(ctx context.Context, tx pgx.Tx, claimID, hostID, deviceID, requestID string, publicKey []byte, protocol uint32) (bool, error) {
	if claimID == "" || hostID == "" || deviceID == "" || requestID == "" {
		return false, nil
	}
	var body []byte
	var generation uint64
	var fingerprint string
	var requestProtocol uint32
	var requestCreated, requestExpires time.Time
	err := tx.QueryRow(ctx, `SELECT r.request_body,o.grant_generation,d.public_key_fingerprint,r.protocol_version,r.created_at,r.expires_at
      FROM requests r JOIN pairing_outcomes o ON o.commit_request_id=r.request_id
      JOIN devices d ON d.device_id=o.device_id
      WHERE o.claim_id=$1 AND o.host_id=$2 AND o.device_id=$3 AND r.request_id=$4
		AND r.host_id=$2 AND r.device_id=$3 AND r.kind=$5`, claimID, hostID, deviceID, requestID, int32(whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE)).Scan(&body, &generation, &fingerprint, &requestProtocol, &requestCreated, &requestExpires)
	if errors.Is(err, pgx.ErrNoRows) {
		return false, nil
	}
	if err != nil {
		return false, err
	}
	var claim whalereinv1.PairingClaim
	if proto.Unmarshal(body, &claim) != nil || len(publicKey) != 32 || protocol == 0 || requestProtocol != protocol || !requestExpires.After(requestCreated) {
		return false, nil
	}
	keyHash := sha256.Sum256(publicKey)
	return claim.Phase == whalereinv1.PairingPhase_PAIRING_PHASE_COMMIT && claim.ClaimId == claimID &&
		claim.DeviceId == deviceID && claim.TokenHash == "" && claim.GrantGeneration == generation && claim.ExpiresAtUnixMs >= requestExpires.UnixMilli() &&
		bytes.Equal(claim.DevicePublicKey, publicKey) && fingerprint == hex.EncodeToString(keyHash[:]), nil
}

// A legacy confirmation requires the matching stored Host acknowledgement; a
// prior boolean alone is not independently sufficient after an upgrade.
func storedPairingAckTx(ctx context.Context, tx pgx.Tx, hostID, deviceID, requestID string) (time.Time, bool, error) {
	var body []byte
	var received time.Time
	err := tx.QueryRow(ctx, `SELECT result_body,updated_at FROM requests WHERE request_id=$1 AND host_id=$2 AND device_id=$3 AND kind=$4 AND status='succeeded' AND updated_at<expires_at AND bodies_cleared IS DISTINCT FROM true`, requestID, hostID, deviceID, int32(whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE)).Scan(&body, &received)
	if errors.Is(err, pgx.ErrNoRows) {
		return time.Time{}, false, nil
	}
	if err != nil {
		return time.Time{}, false, err
	}
	var ack whalereinv1.PairingClaimResult
	if proto.Unmarshal(body, &ack) != nil || !ack.Authorized || ack.DeviceId != deviceID {
		return time.Time{}, false, nil
	}
	return received, true, nil
}

// PairingOutcome loads one committed outcome under the device fence, so the
// active observation is ordered with global and Host-specific revocation.
func (s *Store) PairingOutcome(ctx context.Context, claimID, deviceID string) (PairingOutcome, error) {
	return pairingOutcome(ctx, s.poolBegin, claimID, deviceID)
}

func pairingOutcome(ctx context.Context, begin func(context.Context) (pgx.Tx, error), claimID, deviceID string) (PairingOutcome, error) {
	var out PairingOutcome
	tx, err := begin(ctx)
	if err != nil {
		return out, err
	}
	defer func() { _ = tx.Rollback(ctx) }()
	if _, err = tx.Exec(ctx, `SELECT pg_advisory_xact_lock(hashtextextended('whalerein-device:' || $1, 0))`, deviceID); err != nil {
		return out, err
	}
	var requestID string
	var bindingVerified, confirmationVerified bool
	load := func() error {
		return tx.QueryRow(ctx, `SELECT o.claim_id,o.token_hash,o.host_id,o.device_id,o.finish_hash,o.device_public_key,COALESCE(o.protocol_version,0),o.host_confirmed,o.commit_request_id,o.commit_binding_verified,o.host_confirmation_verified,
      EXISTS(SELECT 1 FROM grants g JOIN device_credentials c ON c.device_id=g.device_id
      WHERE g.host_id=o.host_id AND g.device_id=o.device_id AND g.revoked_at IS NULL
      AND g.grant_generation=o.grant_generation AND c.auth_hash=o.auth_hash)
      FROM pairing_outcomes o WHERE o.claim_id=$1 AND o.device_id=$2`, claimID, deviceID).Scan(&out.ClaimID, &out.TokenHash, &out.HostID, &out.DeviceID, &out.FinishHash, &out.DevicePublicKey, &out.ProtocolVersion, &out.HostConfirmed, &requestID, &bindingVerified, &confirmationVerified, &out.Active)
	}
	if err = load(); err != nil {
		return out, wrapRequestScan(err)
	}
	// Legacy outcomes may have a retained FINISH response but no stored key.
	// Bind only the verified key and COMMIT here; this does not assert a Host
	// acknowledgement and permits safe replay while the outbox is available.
	if !bindingVerified && out.Active && len(out.DevicePublicKey) == 0 && out.ProtocolVersion == 0 {
		var created time.Time
		lookupErr := tx.QueryRow(ctx, `SELECT created_at FROM requests WHERE request_id=$1 AND host_id=$2 AND device_id=$3 AND kind=$4`, requestID, out.HostID, deviceID, int32(whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE)).Scan(&created)
		if lookupErr == nil {
			bound, bindErr := confirmPairingHostTx(ctx, tx, claimID, out.HostID, deviceID, requestID, created, false)
			if bindErr != nil && !errors.Is(bindErr, ErrNotFound) && !errors.Is(bindErr, ErrConflict) {
				return out, bindErr
			}
			if bound {
				if err = load(); err != nil {
					return out, wrapRequestScan(err)
				}
			}
		} else if !errors.Is(lookupErr, pgx.ErrNoRows) {
			return out, lookupErr
		}
	}
	if !bindingVerified {
		bindingVerified, err = verifyPersistedCommitTx(ctx, tx, claimID, out.HostID, deviceID, requestID, out.DevicePublicKey, out.ProtocolVersion)
		if err != nil {
			return out, err
		}
		if bindingVerified {
			result, updateErr := tx.Exec(ctx, `UPDATE pairing_outcomes SET commit_binding_verified=true WHERE claim_id=$1 AND host_id=$2 AND device_id=$3 AND commit_request_id=$4`, claimID, out.HostID, deviceID, requestID)
			if updateErr != nil {
				return out, updateErr
			}
			if result.RowsAffected() != 1 {
				return out, ErrConflict
			}
		}
	}
	// A retained body can contradict an earlier binding proof. Never replay or
	// confirm such a body; an absent body leaves the durable proof intact.
	if bindingVerified {
		var retained bool
		if err = tx.QueryRow(ctx, `SELECT EXISTS(SELECT 1 FROM requests WHERE request_id=$1 AND octet_length(request_body)>0)`, requestID).Scan(&retained); err != nil {
			return out, err
		}
		if retained {
			bindingVerified, err = verifyPersistedCommitTx(ctx, tx, claimID, out.HostID, deviceID, requestID, out.DevicePublicKey, out.ProtocolVersion)
			if err != nil {
				return out, err
			}
		}
	}
	// Old host_confirmed flags lack a proof of the ACK. Preserve the flag,
	// but trust it only after both the currently retained COMMIT and the scoped
	// acknowledgement are verified. A prior binding bit must not mask a
	// contradictory nonempty outbox body.
	if bindingVerified && out.HostConfirmed && !confirmationVerified {
		bound, bindErr := verifyPersistedCommitTx(ctx, tx, claimID, out.HostID, deviceID, requestID, out.DevicePublicKey, out.ProtocolVersion)
		if bindErr != nil {
			return out, bindErr
		}
		if bound {
			_, validAck, ackErr := storedPairingAckTx(ctx, tx, out.HostID, deviceID, requestID)
			if ackErr != nil {
				return out, ackErr
			}
			if validAck {
				result, updateErr := tx.Exec(ctx, `UPDATE pairing_outcomes SET host_confirmation_verified=true WHERE claim_id=$1 AND host_id=$2 AND device_id=$3 AND commit_request_id=$4`, claimID, out.HostID, deviceID, requestID)
				if updateErr != nil {
					return out, updateErr
				}
				if result.RowsAffected() != 1 {
					return out, ErrConflict
				}
				confirmationVerified = true
			}
		}
	}
	if !bindingVerified || !confirmationVerified {
		out.HostConfirmed = false
	}
	// Legacy rows may predate durable confirmation. Recover only a still-stored,
	// valid Host acknowledgement whose original recorded time met its deadline.
	if out.Active && !out.HostConfirmed {
		received, validAck, ackErr := storedPairingAckTx(ctx, tx, out.HostID, deviceID, requestID)
		if ackErr != nil {
			return out, ackErr
		}
		if validAck {
			confirmed, confirmErr := confirmPairingHostTx(ctx, tx, claimID, out.HostID, deviceID, requestID, received, true)
			if confirmErr != nil && !errors.Is(confirmErr, ErrNotFound) && !errors.Is(confirmErr, ErrConflict) {
				return out, confirmErr
			}
			if confirmed {
				if err = load(); err != nil {
					return out, wrapRequestScan(err)
				}
			}
		}
	}
	out.CommitBindingUnproven = !bindingVerified
	if err = tx.Commit(ctx); err != nil {
		return out, err
	}
	return out, nil
}

// reconcilePairingOutcomes preserves any verifiable legacy Host ACK before
// request-body or operation retention cleanup destroys its evidence.
func (s *Store) reconcilePairingOutcomes(ctx context.Context, cutoff time.Time, operationCleanup bool) error {
	return reconcilePairingOutcomes(ctx, s.poolQuery, s.poolBegin, s.PairingOutcome, cutoff, operationCleanup)
}

func reconcilePairingOutcomes(ctx context.Context, query func(context.Context, string, ...any) (pgx.Rows, error), begin func(context.Context) (pgx.Tx, error), observe func(context.Context, string, string) (PairingOutcome, error), cutoff time.Time, operationCleanup bool) error {
	const batchSize = 256 // Internal memory bound; every eligible batch is processed.
	type key struct {
		claim, host, device, request string
		created                      time.Time
		needsBinding                 bool
	}
	after := ""
	for {
		rows, err := query(ctx, `SELECT o.claim_id,o.host_id,o.device_id,o.commit_request_id,r.created_at,(o.device_public_key IS NULL)
		   FROM pairing_outcomes o LEFT JOIN pairing_operations p ON p.claim_id=o.claim_id
		   LEFT JOIN pairing_tokens t ON t.token_hash=p.token_hash
           JOIN requests r ON r.request_id=o.commit_request_id
		   WHERE (NOT o.commit_binding_verified OR (NOT o.host_confirmation_verified AND (o.host_confirmed OR o.device_public_key IS NULL OR (r.status='succeeded' AND octet_length(r.result_body)>0))))
		     AND (($2 AND (p.expires_at<=$1 OR t.expires_at<=$1)) OR (NOT $2 AND r.expires_at<=$1)) AND o.claim_id>$3
           ORDER BY o.claim_id LIMIT $4`, cutoff, operationCleanup, after, batchSize)
		if err != nil {
			return err
		}
		keys := make([]key, 0, batchSize)
		for rows.Next() {
			var item key
			if err = rows.Scan(&item.claim, &item.host, &item.device, &item.request, &item.created, &item.needsBinding); err != nil {
				rows.Close()
				return err
			}
			keys = append(keys, item)
		}
		err = rows.Err()
		rows.Close()
		if err != nil {
			return err
		}
		if len(keys) == 0 {
			return nil
		}
		for _, item := range keys {
			if item.needsBinding {
				tx, beginErr := begin(ctx)
				if beginErr != nil {
					return beginErr
				}
				if _, err = tx.Exec(ctx, `SELECT pg_advisory_xact_lock(hashtextextended('whalerein-device:' || $1, 0))`, item.device); err == nil {
					_, err = confirmPairingHostTx(ctx, tx, item.claim, item.host, item.device, item.request, item.created, false)
				}
				if err == nil {
					err = tx.Commit(ctx)
				} else {
					_ = tx.Rollback(ctx)
				}
				if err != nil && !errors.Is(err, ErrNotFound) && !errors.Is(err, ErrConflict) {
					return err
				}
			}
			if _, err = observe(ctx, item.claim, item.device); err != nil && !errors.Is(err, ErrNotFound) {
				return err
			}
			after = item.claim
		}
		if len(keys) < batchSize {
			return nil
		}
	}
}

// ConfirmPairingHost records a validated Host COMMIT acknowledgement only while
// the exact original grant and credential remain active under the device fence.
func confirmPairingHostTx(ctx context.Context, tx pgx.Tx, claimID, hostID, deviceID, requestID string, at time.Time, markConfirmed bool) (bool, error) {
	if claimID == "" || hostID == "" || deviceID == "" || requestID == "" {
		return false, ErrConflict
	}
	var requestExists bool
	if err := tx.QueryRow(ctx, `SELECT EXISTS(SELECT 1 FROM requests WHERE request_id=$1 AND host_id=$2 AND device_id=$3 AND kind=$4 AND expires_at>$5)`, requestID, hostID, deviceID, int32(whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE), at).Scan(&requestExists); err != nil {
		return false, err
	}
	if !requestExists {
		return false, ErrConflict
	}
	var active, bindingVerified bool
	var publicKey []byte
	var protocol uint32
	err := tx.QueryRow(ctx, `SELECT o.device_public_key,COALESCE(o.protocol_version,0),
      EXISTS(SELECT 1 FROM grants g JOIN device_credentials c ON c.device_id=g.device_id
        WHERE g.host_id=o.host_id AND g.device_id=o.device_id AND g.revoked_at IS NULL
		  AND g.grant_generation=o.grant_generation AND c.auth_hash=o.auth_hash),o.commit_binding_verified
		  FROM pairing_outcomes o WHERE o.claim_id=$1 AND o.host_id=$2 AND o.device_id=$3 AND o.commit_request_id=$4`, claimID, hostID, deviceID, requestID).Scan(&publicKey, &protocol, &active, &bindingVerified)
	if err != nil {
		return false, wrapRequestScan(err)
	}
	if !active {
		return false, nil
	}
	if len(publicKey) == 0 && protocol == 0 {
		var encoded []byte
		err = tx.QueryRow(ctx, `SELECT p.finish_response FROM pairing_operations p JOIN pairing_outcomes o ON o.claim_id=p.claim_id
          WHERE p.claim_id=$1 AND p.host_id=$2 AND p.device_id=$3 AND p.commit_request_id=$4
          AND p.token_hash=o.token_hash AND p.finish_hash=o.finish_hash AND p.grant_generation=o.grant_generation AND p.auth_hash=o.auth_hash`, claimID, hostID, deviceID, requestID).Scan(&encoded)
		if err != nil {
			return false, wrapRequestScan(err)
		}
		var response whalereinv1.PairingClaimResponse
		if proto.Unmarshal(encoded, &response) != nil || response.ClaimId != claimID || response.HostId != hostID || response.DeviceId != deviceID || response.Phase != whalereinv1.PairingPhase_PAIRING_PHASE_FINISH || response.DeviceToken != "" || len(response.DevicePublicKey) != 32 || response.ProtocolVersion == 0 {
			return false, ErrConflict
		}
		publicKey = response.DevicePublicKey
		protocol = response.ProtocolVersion
	}
	if len(publicKey) != 32 || protocol == 0 {
		return false, ErrConflict
	}
	var deviceFingerprint string
	if err = tx.QueryRow(ctx, `SELECT public_key_fingerprint FROM devices WHERE device_id=$1`, deviceID).Scan(&deviceFingerprint); err != nil {
		return false, wrapRequestScan(err)
	}
	fingerprint := sha256.Sum256(publicKey)
	if deviceFingerprint != hex.EncodeToString(fingerprint[:]) {
		return false, ErrConflict
	}
	bound, err := verifyPersistedCommitTx(ctx, tx, claimID, hostID, deviceID, requestID, publicKey, protocol)
	if err != nil {
		return false, err
	}
	if !bound {
		if !bindingVerified {
			return false, ErrConflict
		}
		// A previously verified COMMIT may have been deliberately cleared by
		// retention. A contradictory nonempty body must never use this path.
		var cleaned bool
		if err := tx.QueryRow(ctx, `SELECT EXISTS(SELECT 1 FROM requests WHERE request_id=$1 AND host_id=$2 AND device_id=$3 AND kind=$4 AND protocol_version=$5 AND expires_at>created_at AND bodies_cleared IS TRUE AND octet_length(request_body)=0)`, requestID, hostID, deviceID, int32(whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE), protocol).Scan(&cleaned); err != nil {
			return false, err
		}
		if !cleaned {
			return false, ErrConflict
		}
	}
	result, err := tx.Exec(ctx, `UPDATE pairing_outcomes SET device_public_key=$5,protocol_version=$6,commit_binding_verified=true,host_confirmed=CASE WHEN $7 THEN true ELSE host_confirmed END,host_confirmation_verified=CASE WHEN $7 THEN true ELSE host_confirmation_verified END WHERE claim_id=$1 AND host_id=$2 AND device_id=$3 AND commit_request_id=$4`, claimID, hostID, deviceID, requestID, publicKey, protocol, markConfirmed)
	if err != nil {
		return false, err
	}
	if result.RowsAffected() != 1 {
		return false, ErrConflict
	}
	return true, nil
}

func (s *Store) ConfirmPairingHost(ctx context.Context, claimID, hostID, deviceID, requestID string, at time.Time) (bool, error) {
	return confirmPairingHost(ctx, s.poolBegin, claimID, hostID, deviceID, requestID, at)
}

func confirmPairingHost(ctx context.Context, begin func(context.Context) (pgx.Tx, error), claimID, hostID, deviceID, requestID string, at time.Time) (bool, error) {
	tx, err := begin(ctx)
	if err != nil {
		return false, err
	}
	defer func() { _ = tx.Rollback(ctx) }()
	if _, err = tx.Exec(ctx, `SELECT pg_advisory_xact_lock(hashtextextended('whalerein-device:' || $1, 0))`, deviceID); err != nil {
		return false, err
	}
	confirmed, err := confirmPairingHostTx(ctx, tx, claimID, hostID, deviceID, requestID, at, true)
	if err != nil || !confirmed {
		return confirmed, err
	}
	if err = tx.Commit(ctx); err != nil {
		return false, err
	}
	return true, nil
}
