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
		AND NOT EXISTS(SELECT 1 FROM external_device_revocations r
			WHERE r.host_id=$1 AND r.device_id=$2
			  AND r.generation=(SELECT generation FROM external_enrollments WHERE host_id=$1))`
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
			AND r.generation=e.generation
		WHERE e.workspace_id=$1 AND e.host_id=$2`, workspaceID, hostID, deviceID).Scan(&status)
	if err != nil {
		return "", wrapRequestScan(err)
	}
	if status == nil {
		return "", ErrCloudGrantMissing
	}
	return *status, nil
}

// HostClientIDs returns confirmed, current grants for the exact enrollment.
func (s *Store) HostClientIDs(ctx context.Context, workspaceID, hostID, revision string) ([]string, error) {
	rows, err := s.poolQuery(ctx, `
		SELECT g.device_id
		FROM external_enrollments e
		LEFT JOIN grant_authority g ON g.host_id=e.host_id AND g.active
		  AND e.state='enrollment_ready' AND e.disabled_at IS NULL
		  AND EXISTS (SELECT 1 FROM pairing_outcomes p
			WHERE p.host_id=g.host_id AND p.device_id=g.device_id AND p.grant_generation=g.grant_generation
			  AND p.host_confirmed AND p.host_confirmation_verified AND p.commit_binding_verified
			  AND octet_length(p.token_hash)=33)
		WHERE e.workspace_id=$1 AND e.host_id=$2 AND e.enrollment_revision=$3
		ORDER BY g.device_id`, workspaceID, hostID, revision)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var ids []string
	found := false
	for rows.Next() {
		found = true
		var id *string
		if err := rows.Scan(&id); err != nil {
			return nil, err
		}
		if id != nil {
			ids = append(ids, *id)
		}
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}
	if !found {
		return nil, ErrNotFound
	}
	return ids, nil
}

// CloudDeviceRevocation is the durable, frozen state of one console device
// grant revocation. A retry re-issues exactly these generations and never
// revokes a newer enrollment generation.
type CloudDeviceRevocation struct {
	HostID                     string
	DeviceID                   string
	EnrollmentGeneration       int64
	GrantGeneration            uint64
	RevocationGeneration       uint64
	DeviceRevocationGeneration uint64
	Exists                     bool
	Enforced                   bool
	AppliedConnection          *ConnectionRef
}

// Scope freezes this receipt into the one internal revocation scope. The
// revocation generation advances by one because the durable revoke commits
// before the Broker fence is installed.
func (r CloudDeviceRevocation) Scope() RevocationScope {
	enrollmentGeneration := uint64(0)
	if r.EnrollmentGeneration > 0 {
		enrollmentGeneration = uint64(r.EnrollmentGeneration)
	}
	grant := ScopeGrant{
		HostID:                     r.HostID,
		DeviceID:                   r.DeviceID,
		EnrollmentGeneration:       enrollmentGeneration,
		GrantGeneration:            r.GrantGeneration,
		RevocationGeneration:       r.RevocationGeneration + 1,
		DeviceRevocationGeneration: r.DeviceRevocationGeneration,
	}
	return RevocationScope{
		HostID:               r.HostID,
		DeviceID:             r.DeviceID,
		EnrollmentGeneration: enrollmentGeneration,
		Grants:               []ScopeGrant{grant},
	}
}

// BeginCloudDeviceRevocation records a workspace-scoped fence before any live
// enforcement and returns the frozen generations. It shares the Host lock with
// Cloud pairing commit and disable. A retry with the same enrollment generation
// returns the stored scope instead of re-freezing a newer grant.
func (s *Store) BeginCloudDeviceRevocation(ctx context.Context, workspaceID, hostID, deviceID string, now time.Time) (CloudDeviceRevocation, error) {
	tx, err := s.db.Begin(ctx)
	if err != nil {
		return CloudDeviceRevocation{}, err
	}
	defer func() { _ = tx.Rollback(ctx) }()
	if err := lockCloudHost(ctx, tx, hostID); err != nil {
		return CloudDeviceRevocation{}, err
	}
	var enrollmentGeneration int64
	err = tx.QueryRow(ctx, `
		SELECT generation FROM external_enrollments
		WHERE workspace_id=$1 AND host_id=$2`, workspaceID, hostID).Scan(&enrollmentGeneration)
	if errors.Is(err, pgx.ErrNoRows) {
		return CloudDeviceRevocation{}, ErrNotFound
	}
	if err != nil {
		return CloudDeviceRevocation{}, err
	}
	if _, err = tx.Exec(ctx, `INSERT INTO external_device_revocations
			(host_id,device_id,generation,revoked_at,
			 grant_generation,revocation_generation,device_revocation_generation)
		SELECT $1,$2,$3,$4,
		       COALESCE(g.grant_generation,0),
		       COALESCE(g.revocation_generation,0),
		       COALESCE(d.revocation_generation,0)
		FROM external_enrollments e
		LEFT JOIN grants g ON g.host_id=$1 AND g.device_id=$2
		LEFT JOIN devices d ON d.device_id=$2
		WHERE e.host_id=$1
		ON CONFLICT(host_id,device_id,generation) DO NOTHING`,
		hostID, deviceID, enrollmentGeneration, now); err != nil {
		return CloudDeviceRevocation{}, fmt.Errorf("record Device revocation: %w", err)
	}
	revocation, err := loadCloudDeviceRevocation(ctx, tx, hostID, deviceID, enrollmentGeneration)
	if err != nil {
		return CloudDeviceRevocation{}, err
	}
	if err := tx.Commit(ctx); err != nil {
		return CloudDeviceRevocation{}, err
	}
	return revocation, nil
}

// CompleteCloudDeviceRevocation records successful regional enforcement for the
// current enrollment generation. Deprecated: callers that know the frozen
// generation use CompleteCloudDeviceRevocationAt.
func (s *Store) CompleteCloudDeviceRevocation(ctx context.Context, hostID, deviceID string, now time.Time) error {
	_, err := s.db.Exec(ctx, `UPDATE external_device_revocations SET enforced_at=COALESCE(enforced_at,$3)
		WHERE host_id=$1 AND device_id=$2`, hostID, deviceID, now)
	return err
}

// CompleteCloudDeviceRevocationAt records enforcement evidence for exactly one
// frozen generation. The epoch/id pair is written once and never overwritten.
func (s *Store) CompleteCloudDeviceRevocationAt(ctx context.Context, hostID, deviceID string, enrollmentGeneration int64, applied *ConnectionRef, now time.Time) error {
	var epoch any
	var id any
	if applied != nil && applied.Valid() {
		epoch = applied.Epoch
		id = int64(applied.ID)
	}
	tag, err := s.db.Exec(ctx, `UPDATE external_device_revocations
		SET enforced_at=COALESCE(enforced_at,$4),
		    enforced_connection_epoch=COALESCE(enforced_connection_epoch,$5),
		    enforced_connection_id=COALESCE(enforced_connection_id,$6)
		WHERE host_id=$1 AND device_id=$2 AND generation=$3`,
		hostID, deviceID, enrollmentGeneration, now, epoch, id)
	if err != nil {
		return err
	}
	if tag.RowsAffected() == 0 {
		return ErrNotFound
	}
	return nil
}

func loadCloudDeviceRevocation(ctx context.Context, q querier, hostID, deviceID string, enrollmentGeneration int64) (CloudDeviceRevocation, error) {
	var revocation CloudDeviceRevocation
	var enforcedAt *time.Time
	var epoch *string
	var connectionID *int64
	var grantExists bool
	err := q.QueryRow(ctx, `
		SELECT r.host_id, r.device_id, r.generation, r.grant_generation,
		       r.revocation_generation, r.device_revocation_generation, r.enforced_at,
		       r.enforced_connection_epoch, r.enforced_connection_id,
		       EXISTS(SELECT 1 FROM grants g WHERE g.host_id=r.host_id AND g.device_id=r.device_id)
		FROM external_device_revocations r
		WHERE r.host_id=$1 AND r.device_id=$2 AND r.generation=$3`,
		hostID, deviceID, enrollmentGeneration).Scan(
		&revocation.HostID, &revocation.DeviceID, &revocation.EnrollmentGeneration,
		&revocation.GrantGeneration, &revocation.RevocationGeneration,
		&revocation.DeviceRevocationGeneration, &enforcedAt, &epoch, &connectionID, &grantExists)
	if errors.Is(err, pgx.ErrNoRows) {
		return CloudDeviceRevocation{}, ErrNotFound
	}
	if err != nil {
		return CloudDeviceRevocation{}, err
	}
	revocation.Exists = grantExists
	revocation.Enforced = enforcedAt != nil
	if epoch != nil && connectionID != nil {
		revocation.AppliedConnection = &ConnectionRef{Epoch: *epoch, ID: uint64(*connectionID)}
	}
	return revocation, nil
}
