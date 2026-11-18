package store

import (
	"context"
	"errors"
	"fmt"
	"time"

	"github.com/jackc/pgx/v5"
)

// CloudPairingHashTag distinguishes portable authorization records from
// Standalone invitation SHA-256 digests without exposing a redeemable token.
const CloudPairingHashTag byte = 1

// ErrCloudGrantMissing distinguishes an authorized empty grant from a missing Host.
var ErrCloudGrantMissing = errors.New("Cloud Device grant is missing")

func IsCloudPairingHash(hash []byte) bool {
	return len(hash) == 33 && hash[0] == CloudPairingHashTag
}

func lockCloudHost(ctx context.Context, tx pgx.Tx, hostID string) error {
	_, err := tx.Exec(ctx, `SELECT pg_advisory_xact_lock(hashtextextended('whalerein-host:' || $1, 0))`, hostID)
	return err
}

func verifyCloudPairing(ctx context.Context, tx pgx.Tx, op *PairingOperation, hostID, deviceID string) error {
	if op.CloudSessionID != "" {
		if err := lockActiveDeviceSession(ctx, tx, op.CloudSessionID, deviceID, op.CloudWorkspaceID, servingDeviceSessionAuthority(ctx)); err != nil {
			if errors.Is(err, ErrDeviceSessionEnded) {
				return ErrPairingFenced
			}
			return err
		}
		var bound bool
		err := tx.QueryRow(ctx, `SELECT EXISTS(SELECT 1 FROM pairing_device_sessions
			WHERE claim_id=$1 AND session_id=$2 AND host_id=$3 AND device_id=$4
			AND authority_revision=(SELECT authority_revision FROM device_sessions WHERE session_id=$2))`,
			op.ClaimID, op.CloudSessionID, hostID, deviceID).Scan(&bound)
		if err != nil {
			return err
		}
		if !bound {
			return ErrPairingFenced
		}
	}
	var eligible bool
	eligibility := `
		SELECT EXISTS(SELECT 1 FROM external_enrollments
			WHERE host_id=$1 AND workspace_id=$3 AND enrollment_revision=$4 AND state='enrollment_ready' AND disabled_at IS NULL)
		AND NOT EXISTS(SELECT 1 FROM external_device_revocations WHERE host_id=$1 AND device_id=$2)`
	if op.CloudSessionID == "" {
		eligibility += ` AND NOT EXISTS(SELECT 1 FROM grants WHERE host_id=$1 AND device_id=$2)`
	} else {
		// A terminal, unconfirmed grant incarnation can be replaced. Its
		// generation and exact Host acknowledgments still fence delayed work.
		eligibility += ` AND NOT EXISTS(SELECT 1 FROM grants g WHERE g.host_id=$1 AND g.device_id=$2
			AND (g.revoked_at IS NOT NULL OR NOT EXISTS (
				SELECT 1 FROM pairing_outcomes o
				JOIN pairing_device_sessions p ON p.claim_id=o.claim_id
				JOIN device_sessions prior ON prior.session_id=p.session_id
				WHERE o.host_id=g.host_id AND o.device_id=g.device_id AND o.grant_generation=g.grant_generation
				  AND NOT (o.host_confirmed AND o.host_confirmation_verified AND o.commit_binding_verified)
				  AND (prior.revoked OR p.expires_at<=clock_timestamp() OR prior.authority_revision<>p.authority_revision
				       OR p.authority_revision<>(SELECT authority_revision FROM device_sessions WHERE session_id=$5))
			)))`
	}
	args := []any{hostID, deviceID, op.CloudWorkspaceID, op.CloudEnrollmentRevision}
	if op.CloudSessionID != "" {
		args = append(args, op.CloudSessionID)
	}
	err := tx.QueryRow(ctx, eligibility, args...).Scan(&eligible)
	if err != nil {
		return err
	}
	if !eligible {
		return ErrPairingFenced
	}
	return nil
}

// CloudDeviceGrantStatus observes one scoped grant or an explicit revocation
// fence. Pending distinguishes committed grants from confirmed Host activation
// and durable revocation intents from acknowledged Access enforcement.
func (s *Store) CloudDeviceGrantStatus(ctx context.Context, workspaceID, hostID, deviceID string) (string, error) {
	var status *string
	err := s.db.QueryRow(ctx, `
		SELECT CASE
			WHEN r.enforced_at IS NOT NULL THEN 'revoked'
			WHEN r.revoked_at IS NOT NULL THEN 'pending'
			WHEN g.revoked_at IS NOT NULL THEN 'revoked'
			WHEN g.device_id IS NULL THEN NULL
			WHEN EXISTS (SELECT 1 FROM pairing_outcomes p
				WHERE p.host_id=e.host_id AND p.device_id=g.device_id
				AND p.grant_generation=g.grant_generation AND p.host_confirmed
				AND p.host_confirmation_verified AND p.commit_binding_verified AND octet_length(p.token_hash)=33)
				THEN 'ready'
			ELSE 'pending'
		END
		FROM external_enrollments e
		LEFT JOIN grants g ON g.host_id=e.host_id AND g.device_id=$3
		LEFT JOIN external_device_revocations r ON r.host_id=e.host_id AND r.device_id=$3
		WHERE e.workspace_id=$1 AND e.host_id=$2`, workspaceID, hostID, deviceID).Scan(&status)
	if err != nil {
		return "", wrapRequestScan(err)
	}
	if status == nil {
		return "", ErrCloudGrantMissing
	}
	return *status, nil
}

// BeginCloudDeviceRevocation records a workspace-scoped fence before any live
// enforcement. It shares the Host lock with Cloud pairing commit and disable.
func (s *Store) BeginCloudDeviceRevocation(ctx context.Context, workspaceID, hostID, deviceID string, now time.Time) (bool, error) {
	tx, err := s.db.Begin(ctx)
	if err != nil {
		return false, err
	}
	defer func() { _ = tx.Rollback(ctx) }()
	if err := lockCloudHost(ctx, tx, hostID); err != nil {
		return false, err
	}
	var grantExists bool
	err = tx.QueryRow(ctx, `
		SELECT EXISTS(SELECT 1 FROM grants WHERE host_id=$2 AND device_id=$3)
		FROM external_enrollments WHERE workspace_id=$1 AND host_id=$2`,
		workspaceID, hostID, deviceID).Scan(&grantExists)
	if errors.Is(err, pgx.ErrNoRows) {
		return false, ErrNotFound
	}
	if err != nil {
		return false, err
	}
	_, err = tx.Exec(ctx, `INSERT INTO external_device_revocations(host_id,device_id,revoked_at) VALUES($1,$2,$3)
		ON CONFLICT(host_id,device_id) DO NOTHING`, hostID, deviceID, now)
	if err != nil {
		return false, fmt.Errorf("record Device revocation: %w", err)
	}
	return grantExists, tx.Commit(ctx)
}

// CompleteCloudDeviceRevocation records successful regional enforcement.
func (s *Store) CompleteCloudDeviceRevocation(ctx context.Context, hostID, deviceID string, now time.Time) error {
	_, err := s.db.Exec(ctx, `UPDATE external_device_revocations SET enforced_at=COALESCE(enforced_at,$3)
		WHERE host_id=$1 AND device_id=$2`, hostID, deviceID, now)
	return err
}
