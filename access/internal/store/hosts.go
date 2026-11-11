package store

import (
	"context"
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
	if _, err := s.pool.Exec(ctx, `
		INSERT INTO hosts (host_id) VALUES ($1)
		ON CONFLICT (host_id) DO NOTHING`, hostID); err != nil {
		return Host{}, fmt.Errorf("ensure host: %w", err)
	}
	return s.GetHost(ctx, hostID)
}

// GetHost reads one host.
func (s *Store) GetHost(ctx context.Context, hostID string) (Host, error) {
	row := s.pool.QueryRow(ctx, `
		SELECT host_id, fingerprint, protocol_version, created_at, updated_at
		FROM hosts WHERE host_id = $1`, hostID)
	return scanHost(row)
}

// UpdateHostMetadata stores the host fingerprint and protocol version observed
// during a completed pairing flow.
func (s *Store) UpdateHostMetadata(ctx context.Context, hostID, fingerprint string, protocolVersion uint32, at time.Time) error {
	cmd, err := s.pool.Exec(ctx, `
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
	cmd, err := s.pool.Exec(ctx, `
		UPDATE hosts SET authorization_public_key = $2
		WHERE host_id = $1 AND authorization_public_key IS NULL`, hostID, publicKey)
	if err != nil {
		return fmt.Errorf("set host authorization key: %w", err)
	}
	if cmd.RowsAffected() == 1 {
		return nil
	}
	var existing []byte
	err = s.pool.QueryRow(ctx, `
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
	err := s.pool.QueryRow(ctx, `
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
	rows, err := s.pool.Query(ctx, `
		SELECT h.host_id, h.fingerprint, h.protocol_version, h.created_at, h.updated_at
		FROM hosts h
		JOIN grants g ON g.host_id = h.host_id
		WHERE g.device_id = $1 AND g.revoked_at IS NULL
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

// ActiveDeviceByAuthHash finds a device by the SHA-256 of its transport token,
// requiring at least one live grant.
func (s *Store) ActiveDeviceByAuthHash(ctx context.Context, authHash []byte) (Device, error) {
	row := s.pool.QueryRow(ctx, `
		SELECT d.device_id, d.public_key_fingerprint, c.auth_hash, d.created_at, d.updated_at
        FROM device_credentials c JOIN devices d ON d.device_id=c.device_id
        WHERE c.auth_hash=$1
		  AND EXISTS (SELECT 1 FROM grants g WHERE g.device_id = d.device_id AND g.revoked_at IS NULL)`,
		authHash)
	return scanDevice(row)
}

// DeviceByID reads a device regardless of grant state.
func (s *Store) DeviceByID(ctx context.Context, deviceID string) (Device, error) {
	row := s.pool.QueryRow(ctx, `
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
	err := s.pool.QueryRow(ctx, `
		SELECT EXISTS (
			SELECT 1 FROM grants
			WHERE host_id = $1 AND device_id = $2 AND revoked_at IS NULL
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
	tx, err := s.pool.Begin(ctx)
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
	tx, err := s.pool.Begin(ctx)
	if err != nil {
		return err
	}
	defer func() { _ = tx.Rollback(ctx) }()
	var lockedID string
	if err := tx.QueryRow(ctx, "SELECT device_id FROM devices WHERE device_id=$1 FOR UPDATE", deviceID).Scan(&lockedID); err != nil {
		return wrapRequestScan(err)
	}
	cmd, err := tx.Exec(ctx, `
		UPDATE grants SET revoked_at = $3, updated_at = $3
		WHERE host_id = $1 AND device_id = $2 AND revoked_at IS NULL`, hostID, deviceID, at)
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
	tx, err := s.pool.Begin(ctx)
	if err != nil {
		return nil, false, fmt.Errorf("begin revoke device: %w", err)
	}
	defer func() { _ = tx.Rollback(ctx) }()

	var lockedID string
	if err := tx.QueryRow(ctx, `SELECT device_id FROM devices WHERE device_id=$1 FOR UPDATE`, deviceID).Scan(&lockedID); err != nil {
		if errors.Is(err, pgx.ErrNoRows) {
			return nil, false, nil
		}
		return nil, false, fmt.Errorf("check device: %w", err)
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
	rows, err := s.pool.Query(ctx, `
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
		_, err := s.pool.Exec(ctx, `UPDATE grants SET replayed_at=$4,updated_at=$4
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
	err := s.pool.QueryRow(ctx, `
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

// CommitPairing consumes the invitation and activates the device/grant in one
// transaction. Failure never rotates an existing credential or revokes a grant.
// On success, notice.Body is filled with the same generation bound into its durable outbox row.
func (s *Store) CommitPairing(ctx context.Context, tokenHash []byte, hostID, deviceID, fingerprint string, authHash []byte, at time.Time, notice *whalereinv1.HostRequest, operation *PairingOperation) (bool, error) {
	if operation != nil {
		var response whalereinv1.PairingClaimResponse
		var claim whalereinv1.PairingClaim
		if notice == nil || proto.Unmarshal(operation.FinishResponse, &response) != nil || proto.Unmarshal(notice.Body, &claim) != nil ||
			response.DeviceToken != "" || len(operation.FinishHash) != 32 || response.ClaimId != operation.ClaimID ||
			claim.ClaimId != operation.ClaimID || response.HostId != hostID || response.DeviceId != deviceID ||
			response.Phase != whalereinv1.PairingPhase_PAIRING_PHASE_FINISH || len(response.DevicePublicKey) != 32 {
			return false, ErrConflict
		}
	}
	tx, err := s.pool.Begin(ctx)
	if err != nil {
		return false, err
	}
	defer func() { _ = tx.Rollback(ctx) }()
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
