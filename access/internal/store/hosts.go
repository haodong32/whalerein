package store

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"github.com/jackc/pgx/v5"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
	"time"
)

// EnsureHost inserts a host on its first broker authentication and refreshes
// its last-seen timestamp afterwards. Presence still comes only from the
// broker, never from this timestamp.
func (s *Store) EnsureHost(ctx context.Context, hostID string) (Host, error) {
	if hostID == "" {
		return Host{}, errors.New("host_id is required")
	}
	if _, err := s.db.Exec(ctx, `
		INSERT INTO hosts (host_id) VALUES ($1)
		ON CONFLICT (host_id) DO NOTHING`, hostID); err != nil {
		return Host{}, fmt.Errorf("ensure host: %w", err)
	}
	return s.GetHost(ctx, hostID)
}

// GetHost reads one host.
func (s *Store) GetHost(ctx context.Context, hostID string) (Host, error) {
	row := s.db.QueryRow(ctx, `
		SELECT host_id, fingerprint, protocol_version, created_at, updated_at
		FROM hosts WHERE host_id = $1`, hostID)
	return scanHost(row)
}

// UpdateHostMetadata stores the host fingerprint and protocol version observed
// during a completed pairing flow.
func (s *Store) UpdateHostMetadata(ctx context.Context, hostID, fingerprint string, protocolVersion uint32, at time.Time) error {
	cmd, err := s.db.Exec(ctx, `
		UPDATE hosts SET fingerprint = $2, protocol_version = $3, updated_at = $4
		WHERE host_id = $1`, hostID, fingerprint, int32(protocolVersion), at)
	if err != nil {
		return fmt.Errorf("update host metadata: %w", err)
	}
	if cmd.RowsAffected() == 0 {
		return ErrNotFound
	}
	return nil
}

// SetHostAuthorizationKey stores the host Ed25519 public key once.
// A different key presented later is ignored. A missing host returns ErrNotFound.
func (s *Store) SetHostAuthorizationKey(ctx context.Context, hostID string, publicKey []byte) error {
	if hostID == "" || len(publicKey) != 32 {
		return fmt.Errorf("set host authorization key: %w", ErrNotFound)
	}
	cmd, err := s.db.Exec(ctx, `
		UPDATE hosts SET authorization_public_key = $2
		WHERE host_id = $1 AND authorization_public_key IS NULL`, hostID, publicKey)
	if err != nil {
		return fmt.Errorf("set host authorization key: %w", err)
	}
	if cmd.RowsAffected() == 1 {
		return nil
	}
	var existing []byte
	err = s.db.QueryRow(ctx, `
		SELECT authorization_public_key FROM hosts WHERE host_id = $1`, hostID).Scan(&existing)
	if err != nil {
		if errors.Is(wrapRequestScan(err), ErrNotFound) {
			return ErrNotFound
		}
		return fmt.Errorf("read host authorization key: %w", err)
	}
	return nil
}

// HostAuthorizationKey returns the registered Ed25519 public key.
// A missing host or a host that has not registered a key returns ErrNotFound.
func (s *Store) HostAuthorizationKey(ctx context.Context, hostID string) ([]byte, error) {
	var key []byte
	err := s.db.QueryRow(ctx, `
		SELECT authorization_public_key FROM hosts WHERE host_id = $1`, hostID).Scan(&key)
	if err != nil {
		if errors.Is(wrapRequestScan(err), ErrNotFound) {
			return nil, ErrNotFound
		}
		return nil, fmt.Errorf("read host authorization key: %w", err)
	}
	if len(key) != 32 {
		return nil, ErrNotFound
	}
	return key, nil
}

func scanHost(row interface{ Scan(...any) error }) (Host, error) {
	var host Host
	var protocol int32
	err := row.Scan(&host.HostID, &host.Fingerprint, &protocol, &host.CreatedAt, &host.UpdatedAt)
	if err != nil {
		return Host{}, wrapRequestScan(err)
	}
	host.ProtocolVersion = uint32(protocol)
	return host, nil
}

// ListAuthorizedHosts returns every host with a live grant for the device.
func (s *Store) ListAuthorizedHosts(ctx context.Context, deviceID string) ([]Host, error) {
	rows, err := s.db.Query(ctx, `
		SELECT h.host_id, h.fingerprint, h.protocol_version, h.created_at, h.updated_at
		FROM hosts h
		JOIN grant_authority g ON g.host_id = h.host_id
		WHERE g.device_id = $1 AND g.active
		ORDER BY h.host_id`, deviceID)
	if err != nil {
		return nil, fmt.Errorf("list authorized hosts: %w", err)
	}
	defer rows.Close()
	hosts := make([]Host, 0)
	for rows.Next() {
		host, err := scanHost(rows)
		if err != nil {
			return nil, err
		}
		hosts = append(hosts, host)
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("list authorized hosts: %w", err)
	}
	return hosts, nil
}

// ListAuthorizedHostsPage returns one bounded page of the hosts a device may
// access. Ordering by host_id makes limit/offset paging stable.
func (s *Store) ListAuthorizedHostsPage(ctx context.Context, deviceID string, limit, offset int) ([]Host, error) {
	if limit <= 0 {
		return nil, errors.New("list authorized hosts: limit must be positive")
	}
	if offset < 0 {
		return nil, errors.New("list authorized hosts: offset must not be negative")
	}
	rows, err := s.db.Query(ctx, `
		SELECT h.host_id, h.fingerprint, h.protocol_version, h.created_at, h.updated_at
		FROM hosts h
		JOIN grant_authority g ON g.host_id = h.host_id
		WHERE g.device_id = $1 AND g.active
		ORDER BY h.host_id
		LIMIT $2 OFFSET $3`, deviceID, limit, offset)
	if err != nil {
		return nil, fmt.Errorf("list authorized hosts: %w", err)
	}
	defer rows.Close()
	hosts := make([]Host, 0, limit)
	for rows.Next() {
		host, err := scanHost(rows)
		if err != nil {
			return nil, err
		}
		hosts = append(hosts, host)
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("list authorized hosts: %w", err)
	}
	return hosts, nil
}

// ListAuthorizedHostIDs returns the ids of every host with a live grant for the
// device. Broker presence uses it to publish one event per authorized host.
func (s *Store) ListAuthorizedHostIDs(ctx context.Context, deviceID string) ([]string, error) {
	hosts, err := s.ListAuthorizedHosts(ctx, deviceID)
	if err != nil {
		return nil, err
	}
	hostIDs := make([]string, 0, len(hosts))
	for _, host := range hosts {
		hostIDs = append(hostIDs, host.HostID)
	}
	return hostIDs, nil
}

// ActiveDeviceByAuthHash finds a device by the SHA-256 of its transport token,
// requiring at least one live grant.
func (s *Store) ActiveDeviceByAuthHash(ctx context.Context, authHash []byte) (Device, error) {
	row := s.db.QueryRow(ctx, `
		SELECT d.device_id, d.public_key_fingerprint, c.auth_hash, d.created_at, d.updated_at
        FROM device_credentials c JOIN devices d ON d.device_id=c.device_id
        WHERE c.auth_hash=$1
		  AND EXISTS (SELECT 1 FROM grant_authority g WHERE g.device_id = d.device_id AND g.active)
		  AND NOT EXISTS (
			SELECT 1 FROM pairing_outcomes p
			WHERE p.device_id=d.device_id AND p.auth_hash=c.auth_hash AND octet_length(p.token_hash)=33
			AND NOT (p.host_confirmed AND p.host_confirmation_verified AND p.commit_binding_verified)
		  )`,
		authHash)
	return scanDevice(row)
}

// DeviceByID reads a device regardless of grant state.
func (s *Store) DeviceByID(ctx context.Context, deviceID string) (Device, error) {
	row := s.db.QueryRow(ctx, `
		SELECT d.device_id, d.public_key_fingerprint,
            (SELECT c.auth_hash FROM device_credentials c WHERE c.device_id=d.device_id ORDER BY c.created_at DESC,c.auth_hash LIMIT 1),
            d.created_at,d.updated_at FROM devices d WHERE d.device_id=$1`, deviceID)
	return scanDevice(row)
}

func scanDevice(row interface{ Scan(...any) error }) (Device, error) {
	var device Device
	err := row.Scan(&device.DeviceID, &device.PublicKeyFingerprint, &device.AuthHash,
		&device.CreatedAt, &device.UpdatedAt)
	if err != nil {
		return Device{}, wrapRequestScan(err)
	}
	return device, nil
}

// HasActiveGrant reports whether the device currently may access the host.
func (s *Store) HasActiveGrant(ctx context.Context, hostID, deviceID string) (bool, error) {
	var exists bool
	err := s.db.QueryRow(ctx, `
		SELECT EXISTS (
			SELECT 1 FROM grant_authority
			WHERE host_id = $1 AND device_id = $2 AND active
		)`, hostID, deviceID).Scan(&exists)
	if err != nil {
		return false, fmt.Errorf("check grant: %w", err)
	}
	return exists, nil
}

// CreatePairedDevice writes devices and grants in one transaction after the
// host confirmed FINISH. Each pairing adds a credential without invalidating
// other live Host enrollments and explicitly renews this Host/device grant.
func (s *Store) CreatePairedDevice(ctx context.Context, hostID, deviceID, publicKeyFingerprint string, authHash []byte, at time.Time) error {
	tx, err := s.db.Begin(ctx)
	if err != nil {
		return fmt.Errorf("begin register device: %w", err)
	}
	defer func() { _ = tx.Rollback(ctx) }()

	if err := writePairedDevice(ctx, tx, hostID, deviceID, publicKeyFingerprint, authHash, at); err != nil {
		return err
	}

	if err := tx.Commit(ctx); err != nil {
		return fmt.Errorf("commit register device: %w", err)
	}
	return nil
}

// RevokeGrant marks one host's grant revoked after an authorized owner action.
// Failed pairing transactions roll back and never invoke this operation.
func (s *Store) RevokeGrant(ctx context.Context, hostID, deviceID string, at time.Time) error {
	return revokeGrant(ctx, s.poolBegin, hostID, deviceID, at)
}

func revokeGrant(ctx context.Context, begin func(context.Context) (pgx.Tx, error), hostID, deviceID string, at time.Time) error {
	tx, err := begin(ctx)
	if err != nil {
		return err
	}
	defer func() { _ = tx.Rollback(ctx) }()
	if _, err := tx.Exec(ctx, `SELECT pg_advisory_xact_lock(hashtextextended('whalerein-device:' || $1, 0))`, deviceID); err != nil {
		return err
	}
	var lockedID string
	if err := tx.QueryRow(ctx, "SELECT device_id FROM devices WHERE device_id=$1 FOR UPDATE", deviceID).Scan(&lockedID); err != nil {
		return wrapRequestScan(err)
	}
	cmd, err := tx.Exec(ctx, `
		UPDATE grants SET revoked_at = COALESCE(revoked_at, $3), updated_at = $3, revocation_generation = revocation_generation + 1
		WHERE host_id = $1 AND device_id = $2`, hostID, deviceID, at)
	if err != nil {
		return fmt.Errorf("revoke grant: %w", err)
	}
	if cmd.RowsAffected() == 0 {
		// Already revoked is an acceptable compensation result.
		var exists bool
		if err := tx.QueryRow(ctx, `
			SELECT EXISTS (SELECT 1 FROM grants WHERE host_id = $1 AND device_id = $2)`,
			hostID, deviceID).Scan(&exists); err != nil {
			return fmt.Errorf("check grant for revoke: %w", err)
		}
		if !exists {
			return ErrNotFound
		}
	}
	if _, err := tx.Exec(ctx, `DELETE FROM device_credentials WHERE device_id=$1 AND NOT EXISTS(SELECT 1 FROM grants WHERE device_id=$1 AND revoked_at IS NULL)`, deviceID); err != nil {
		return err
	}
	return tx.Commit(ctx)
}

// RevokeDeviceGrants revokes every live grant of a device in one transaction.
// It returns the affected host ids and whether the device exists. The ACL is
// database-derived, so the revocation is effective at commit.
func (s *Store) RevokeDeviceGrants(ctx context.Context, deviceID string, at time.Time) ([]string, bool, error) {
	return revokeDeviceGrants(ctx, s.poolBegin, deviceID, at)
}

func revokeDeviceGrants(ctx context.Context, begin func(context.Context) (pgx.Tx, error), deviceID string, at time.Time) ([]string, bool, error) {
	tx, err := begin(ctx)
	if err != nil {
		return nil, false, fmt.Errorf("begin revoke device: %w", err)
	}
	defer func() { _ = tx.Rollback(ctx) }()

	// All pairing and global-revocation paths acquire the same device fence first.
	if _, err := tx.Exec(ctx, `SELECT pg_advisory_xact_lock(hashtextextended('whalerein-device:' || $1, 0))`, deviceID); err != nil {
		return nil, false, err
	}
	var lockedID string
	if err := tx.QueryRow(ctx, `SELECT device_id FROM devices WHERE device_id=$1 FOR UPDATE`, deviceID).Scan(&lockedID); err != nil {
		if errors.Is(err, pgx.ErrNoRows) {
			return nil, false, nil
		}
		return nil, false, fmt.Errorf("check device: %w", err)
	}

	if _, err := tx.Exec(ctx, `UPDATE devices SET revocation_generation=revocation_generation+1 WHERE device_id=$1`, deviceID); err != nil {
		return nil, true, err
	}
	rows, err := tx.Query(ctx, `
		WITH revoked AS (
			UPDATE grants SET revoked_at = $2, updated_at = $2
			WHERE device_id = $1 AND revoked_at IS NULL
			RETURNING host_id
		)
		SELECT DISTINCT host_id FROM revoked ORDER BY host_id`, deviceID, at)
	if err != nil {
		return nil, true, fmt.Errorf("revoke device grants: %w", err)
	}
	hosts := make([]string, 0)
	for rows.Next() {
		var hostID string
		if err := rows.Scan(&hostID); err != nil {
			rows.Close()
			return nil, true, err
		}
		hosts = append(hosts, hostID)
	}
	rows.Close()
	if err := rows.Err(); err != nil {
		return nil, true, fmt.Errorf("revoke device grants: %w", err)
	}
	if _, err := tx.Exec(ctx, "DELETE FROM device_credentials WHERE device_id=$1", deviceID); err != nil {
		return nil, true, err
	}
	if err := tx.Commit(ctx); err != nil {
		return nil, true, fmt.Errorf("commit revoke device: %w", err)
	}
	return hosts, true, nil
}

// PendingRevocations returns every unacknowledged revocation for a host.
func (s *Store) PendingRevocations(ctx context.Context, hostID string) ([]Revocation, error) {
	rows, err := s.db.Query(ctx, `
		SELECT device_id, revoked_at, grant_generation FROM grants
		WHERE host_id = $1 AND revoked_at IS NOT NULL AND replayed_at IS NULL
		ORDER BY revoked_at, device_id`, hostID)
	if err != nil {
		return nil, fmt.Errorf("list pending revocations: %w", err)
	}
	defer rows.Close()
	out := make([]Revocation, 0)
	for rows.Next() {
		var rev Revocation
		if err := rows.Scan(&rev.DeviceID, &rev.RevokedAt, &rev.GrantGeneration); err != nil {
			return nil, err
		}
		rev.HostID = hostID
		out = append(out, rev)
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("list pending revocations: %w", err)
	}
	return out, nil
}

// MarkRevocationsReplayed records the host acknowledgement. Only revocations
// that were already pending at the acknowledgement time are cleared, so a
// concurrent re-revocation stays pending.
func (s *Store) MarkRevocationsReplayed(ctx context.Context, hostID string, deviceIDs []string, revokedBefore, at time.Time, generations map[string]uint64) error {
	if len(deviceIDs) == 0 {
		return nil
	}
	for _, deviceID := range deviceIDs {
		_, err := s.db.Exec(ctx, `UPDATE grants SET replayed_at=$4,updated_at=$4
            WHERE host_id=$1 AND device_id=$2 AND revoked_at IS NOT NULL
              AND replayed_at IS NULL AND revoked_at <= $3 AND grant_generation=$5`,
			hostID, deviceID, revokedBefore, at, generations[deviceID])
		if err != nil {
			return fmt.Errorf("mark revocations replayed: %w", err)
		}
	}
	return nil
}

// CountActiveGrants is a small component-test and diagnostics helper.
func (s *Store) CountActiveGrants(ctx context.Context, deviceID string) (int, error) {
	var count int
	err := s.db.QueryRow(ctx, `
		SELECT count(*) FROM grants WHERE device_id = $1 AND revoked_at IS NULL`, deviceID).Scan(&count)
	if err != nil {
		return 0, fmt.Errorf("count active grants: %w", err)
	}
	return count, nil
}

// writePairedDevice never rebinds an existing device ID to another public key.
func writePairedDevice(ctx context.Context, tx pgx.Tx, hostID, deviceID, publicKeyFingerprint string, authHash []byte, at time.Time) error {
	if command, err := tx.Exec(ctx, `
		INSERT INTO devices (device_id, public_key_fingerprint, created_at, updated_at)
        VALUES ($1, $2, $3, $3)
		ON CONFLICT (device_id) DO UPDATE
		SET public_key_fingerprint = EXCLUDED.public_key_fingerprint,
		    updated_at = EXCLUDED.updated_at
        WHERE devices.public_key_fingerprint = EXCLUDED.public_key_fingerprint`,
		deviceID, publicKeyFingerprint, at); err != nil {
		if isUniqueViolation(err) {
			return fmt.Errorf("register device: %w", ErrConflict)
		}
		return fmt.Errorf("register device: %w", err)
	} else if command.RowsAffected() == 0 {
		return ErrConflict
	}

	// A repair of the only Host rotates credentials. Other live Hosts retain
	// their device credentials until an explicit global revocation.
	if _, err := tx.Exec(ctx, `DELETE FROM device_credentials WHERE device_id=$1 AND NOT EXISTS(
        SELECT 1 FROM grants WHERE device_id=$1 AND host_id<>$2 AND revoked_at IS NULL)`, deviceID, hostID); err != nil {
		return err
	}
	if command, err := tx.Exec(ctx, `INSERT INTO device_credentials(auth_hash,device_id,created_at) VALUES($1,$2,$3)
        ON CONFLICT(auth_hash) DO UPDATE SET device_id=EXCLUDED.device_id WHERE device_credentials.device_id=EXCLUDED.device_id`, authHash, deviceID, at); err != nil {
		return fmt.Errorf("store device credential: %w", err)
	} else if command.RowsAffected() != 1 {
		return ErrConflict
	}

	if _, err := tx.Exec(ctx, `
		INSERT INTO grants (host_id, device_id, created_at, updated_at, revoked_at, replayed_at, grant_generation)
		VALUES ($1, $2, $3, $3, NULL, NULL, 1)
		ON CONFLICT (host_id, device_id) DO UPDATE
		SET updated_at = EXCLUDED.updated_at, revoked_at = NULL, replayed_at = NULL, grant_generation = grants.grant_generation + 1`,
		hostID, deviceID, at); err != nil {
		return fmt.Errorf("create grant: %w", err)
	}

	return nil
}

func verifyPairingFences(ctx context.Context, tx pgx.Tx, operation *PairingOperation, hostID, deviceID string) (bool, error) {
	var err error
	if _, err = tx.Exec(ctx, `SELECT pg_advisory_xact_lock(hashtextextended('whalerein-device:' || $1, 0))`, deviceID); err != nil {
		return false, err
	}
	var generation, captured, hostGeneration, hostCaptured int64
	var finished []byte
	err = tx.QueryRow(ctx, `SELECT COALESCE((SELECT revocation_generation FROM devices WHERE device_id=$1),0)`, deviceID).Scan(&generation)
	if err != nil {
		return false, err
	}
	err = tx.QueryRow(ctx, `SELECT device_generation,host_generation,finish_hash FROM pairing_operations WHERE claim_id=$1 AND device_id=$2 AND host_id=$3`, operation.ClaimID, deviceID, hostID).Scan(&captured, &hostCaptured, &finished)
	if err != nil {
		return false, err
	}
	if len(finished) != 0 {
		return false, nil
	}
	err = tx.QueryRow(ctx, `SELECT COALESCE((SELECT revocation_generation FROM grants WHERE host_id=$1 AND device_id=$2),0)`, hostID, deviceID).Scan(&hostGeneration)
	if err != nil {
		return false, err
	}
	if generation != captured || hostGeneration != hostCaptured {
		return false, ErrPairingFenced
	}
	return true, nil
}

// CommitPairing consumes the invitation and activates the device/grant in one
// transaction. Failure never rotates an existing credential or revokes a grant.
// On success, notice.Body is filled with the same generation bound into its durable outbox row.
func (s *Store) CommitPairing(ctx context.Context, tokenHash []byte, hostID, deviceID, fingerprint string, authHash []byte, at time.Time, notice *whalereinv1.HostRequest, operation *PairingOperation) (bool, error) {
	return commitPairing(ctx, s.poolBegin, tokenHash, hostID, deviceID, fingerprint, authHash, at, notice, operation)
}

func commitPairing(ctx context.Context, begin func(context.Context) (pgx.Tx, error), tokenHash []byte, hostID, deviceID, fingerprint string, authHash []byte, at time.Time, notice *whalereinv1.HostRequest, operation *PairingOperation) (bool, error) {
	// Every durable grant must derive from a fenced HELLO operation.
	if operation == nil {
		return false, ErrConflict
	}
	var response whalereinv1.PairingClaimResponse
	if operation != nil {
		var claim whalereinv1.PairingClaim
		if notice == nil || proto.Unmarshal(operation.FinishResponse, &response) != nil || proto.Unmarshal(notice.Body, &claim) != nil ||
			response.DeviceToken != "" || len(operation.FinishHash) != 32 || response.ClaimId != operation.ClaimID ||
			claim.ClaimId != operation.ClaimID || response.HostId != hostID || response.DeviceId != deviceID ||
			response.Phase != whalereinv1.PairingPhase_PAIRING_PHASE_FINISH || len(response.DevicePublicKey) != 32 ||
			claim.Phase != whalereinv1.PairingPhase_PAIRING_PHASE_COMMIT || claim.DeviceId != deviceID ||
			claim.TokenHash != "" || !bytes.Equal(claim.DevicePublicKey, response.DevicePublicKey) ||
			notice.RequestId == "" || notice.ExpiresAtUnixMs <= at.UnixMilli() || claim.ExpiresAtUnixMs < notice.ExpiresAtUnixMs ||
			notice.ProtocolVersion == 0 || notice.ProtocolVersion != response.ProtocolVersion {
			return false, ErrConflict
		}
		keyHash := sha256.Sum256(response.DevicePublicKey)
		if fingerprint != hex.EncodeToString(keyHash[:]) {
			return false, ErrConflict
		}
	}
	tx, err := begin(ctx)
	if err != nil {
		return false, err
	}
	defer func() { _ = tx.Rollback(ctx) }()
	cloud := IsCloudPairingHash(tokenHash)
	if cloud {
		if err := lockCloudHost(ctx, tx, hostID); err != nil {
			return false, err
		}
	}
	proceed, err := verifyPairingFences(ctx, tx, operation, hostID, deviceID)
	if err != nil || !proceed {
		return false, err
	}
	if cloud {
		if err := verifyCloudPairing(ctx, tx, operation, hostID, deviceID); err != nil {
			return false, err
		}
	}

	command, err := tx.Exec(ctx, `UPDATE pairing_tokens SET used_at=$2,updated_at=$2 WHERE token_hash=$1 AND host_id=$3 AND used_at IS NULL AND expires_at>$2`, tokenHash, at, hostID)
	if err != nil {
		return false, err
	}
	if command.RowsAffected() != 1 {
		return false, nil
	}
	if err := writePairedDevice(ctx, tx, hostID, deviceID, fingerprint, authHash, at); err != nil {
		return false, err
	}
	var committedBody []byte
	if notice != nil {
		if notice.HostId != hostID || notice.DeviceId != deviceID || notice.Kind != whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE {
			return false, ErrConflict
		}
		var generation uint64
		if err := tx.QueryRow(ctx, "SELECT grant_generation FROM grants WHERE host_id=$1 AND device_id=$2", hostID, deviceID).Scan(&generation); err != nil {
			return false, err
		}
		var claim whalereinv1.PairingClaim
		if err := proto.Unmarshal(notice.Body, &claim); err != nil {
			return false, ErrConflict
		}
		claim.GrantGeneration = generation
		committedBody, err = proto.Marshal(&claim)
		if err != nil {
			return false, err
		}
		_, err = tx.Exec(ctx, `INSERT INTO requests (request_id,host_id,device_id,kind,protocol_version,status,request_body,expires_at,trace_id,forward_after,created_at,updated_at) VALUES ($1,$2,$3,$4,$5,'received',$6,$7,$8,$9,$9,$9)`, notice.RequestId, hostID, deviceID, notice.Kind, notice.ProtocolVersion, committedBody, time.UnixMilli(notice.ExpiresAtUnixMs), notice.TraceId, at)
		if err != nil {
			return false, err
		}
		if operation.CloudSessionID != "" {
			if _, err = tx.Exec(ctx, `UPDATE requests SET device_session_id=$2,
				device_session_authority=(SELECT authority_revision FROM device_sessions WHERE session_id=$2)
				WHERE request_id=$1`,
				notice.RequestId, operation.CloudSessionID); err != nil {
				return false, err
			}
		}
	}
	if operation != nil {
		op := operation
		var generation uint64
		if err := tx.QueryRow(ctx, "SELECT grant_generation FROM grants WHERE host_id=$1 AND device_id=$2", hostID, deviceID).Scan(&generation); err != nil {
			return false, err
		}
		cmd, err := tx.Exec(ctx, `UPDATE pairing_operations SET finish_hash=$2,finish_response=$3,commit_request_id=$4,grant_generation=$5,auth_hash=$6
            WHERE claim_id=$1 AND token_hash=$7 AND host_id=$8 AND device_id=$9 AND finish_hash IS NULL AND expires_at>$10`,
			op.ClaimID, op.FinishHash, op.FinishResponse, notice.RequestId, generation, authHash, tokenHash, hostID, deviceID, at)
		if err != nil {
			return false, err
		}
		if cmd.RowsAffected() != 1 {
			return false, ErrConflict
		}
		_, err = tx.Exec(ctx, `INSERT INTO pairing_outcomes(claim_id,token_hash,host_id,device_id,finish_hash,grant_generation,auth_hash,commit_request_id,device_public_key,protocol_version,commit_binding_verified)
		  VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,true)`, op.ClaimID, tokenHash, hostID, deviceID, op.FinishHash, generation, authHash, notice.RequestId, response.DevicePublicKey, response.ProtocolVersion)
		if err != nil {
			return false, err
		}
	}
	// The device fence and other PostgreSQL row locks may have delayed this
	// transaction after the caller sampled its clock. Check real database time
	// immediately before commit so the original deadlines cannot be extended.
	var databaseNow, tokenExpiry, operationExpiry time.Time
	if err := tx.QueryRow(ctx, `SELECT clock_timestamp(),t.expires_at,p.expires_at
		FROM pairing_tokens t JOIN pairing_operations p ON p.token_hash=t.token_hash
		WHERE t.token_hash=$1 AND t.host_id=$2 AND p.claim_id=$3 AND p.host_id=$2 AND p.device_id=$4`, tokenHash, hostID, operation.ClaimID, deviceID).Scan(&databaseNow, &tokenExpiry, &operationExpiry); err != nil {
		return false, err
	}
	if databaseNow.Before(at) {
		databaseNow = at
	}
	// A database clock behind Access must not extend the caller's deadline.
	if applicationNow := time.Now().UTC(); databaseNow.Before(applicationNow) {
		databaseNow = applicationNow
	}
	if !databaseNow.Before(tokenExpiry) || (cloud && !databaseNow.Before(operation.CloudAuthorizationExpiry)) {
		return false, ErrPairingInvitationExpired
	}
	if !databaseNow.Before(operationExpiry) || databaseNow.UnixMilli() >= notice.ExpiresAtUnixMs {
		return false, ErrPairingIntentExpired
	}
	if err := tx.Commit(ctx); err != nil {
		return false, err
	}
	// The direct publish must carry exactly the generation persisted in the outbox.
	if notice != nil {
		notice.Body = committedBody
	}
	return true, nil
}

// AdmissionGrant is the authorization record the realtime admission cache
// needs. Generation values let callers detect superseded loads and revocations.
type AdmissionGrant struct {
	HostID                     string
	DeviceID                   string
	Active                     bool
	GrantGeneration            uint64
	RevocationGeneration       uint64
	DeviceRevocationGeneration uint64
}

// AdmissionGrantFor reads one grant revision, including revoked rows so a
// stale cache entry cannot be mistaken for current authority.
func (s *Store) AdmissionGrantFor(ctx context.Context, hostID, deviceID string) (AdmissionGrant, error) {
	row := s.db.QueryRow(ctx, `
		SELECT g.host_id, g.device_id, g.active, g.grant_generation,
		       g.revocation_generation, d.revocation_generation
		FROM grant_authority g JOIN devices d ON d.device_id = g.device_id
		WHERE g.host_id = $1 AND g.device_id = $2`, hostID, deviceID)
	grant, err := scanAdmissionGrant(row)
	if err != nil {
		return AdmissionGrant{}, err
	}
	return grant, nil
}

// ListAdmissionGrants reads every grant revision for startup and periodic
// reconciliation. Rows are ordered for deterministic tests.
func (s *Store) ListAdmissionGrants(ctx context.Context) ([]AdmissionGrant, error) {
	rows, err := s.db.Query(ctx, `
		SELECT g.host_id, g.device_id, g.active, g.grant_generation,
		       g.revocation_generation, d.revocation_generation
		FROM grant_authority g JOIN devices d ON d.device_id = g.device_id
		ORDER BY g.host_id, g.device_id`)
	if err != nil {
		return nil, fmt.Errorf("list admission grants: %w", err)
	}
	defer rows.Close()
	grants := make([]AdmissionGrant, 0)
	for rows.Next() {
		grant, err := scanAdmissionGrant(rows)
		if err != nil {
			return nil, err
		}
		grants = append(grants, grant)
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("list admission grants: %w", err)
	}
	return grants, nil
}

func scanAdmissionGrant(row interface{ Scan(...any) error }) (AdmissionGrant, error) {
	var grant AdmissionGrant
	err := row.Scan(&grant.HostID, &grant.DeviceID, &grant.Active, &grant.GrantGeneration,
		&grant.RevocationGeneration, &grant.DeviceRevocationGeneration)
	if err != nil {
		return AdmissionGrant{}, wrapRequestScan(err)
	}
	return grant, nil
}

// ListDeviceGrants reads every grant revision for one device. Broker policy
// projection uses it when a device connects.
func (s *Store) ListDeviceGrants(ctx context.Context, deviceID string) ([]AdmissionGrant, error) {
	rows, err := s.db.Query(ctx, `
		SELECT g.host_id, g.device_id, g.active, g.grant_generation,
		       g.revocation_generation, d.revocation_generation
		FROM grant_authority g JOIN devices d ON d.device_id = g.device_id
		WHERE g.device_id = $1
		ORDER BY g.host_id`, deviceID)
	if err != nil {
		return nil, fmt.Errorf("list device grants: %w", err)
	}
	defer rows.Close()
	grants := make([]AdmissionGrant, 0)
	for rows.Next() {
		grant, err := scanAdmissionGrant(rows)
		if err != nil {
			return nil, err
		}
		grants = append(grants, grant)
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("list device grants: %w", err)
	}
	return grants, nil
}
