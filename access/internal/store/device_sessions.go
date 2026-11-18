package store

import (
	"context"
	"errors"
	"time"

	"github.com/jackc/pgx/v5"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// DeviceSession describes one installed regional permission.
type DeviceSession struct {
	ID, WorkspaceID, RegionID, DeviceID, KeyFingerprint, AuthorityRevision string
	CredentialHash                                                         []byte
	Revision                                                               int64
	ExpiresAt                                                              time.Time
}

// InstallDeviceSession changes only the revision and deadline on renewal.
// An irreversible session fence wins even if the initial permit is in flight.
func (s *Store) InstallDeviceSession(ctx context.Context, permission DeviceSession) error {
	tag, err := s.db.Exec(ctx, `
		INSERT INTO device_sessions (session_id,workspace_id,region_id,device_id,key_fingerprint,
			authority_revision, credential_hash,revision,expires_at)
		VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
		ON CONFLICT (session_id) DO UPDATE SET revision=excluded.revision,expires_at=excluded.expires_at,
			authority_revision=excluded.authority_revision
		WHERE NOT device_sessions.revoked
			AND device_sessions.workspace_id=excluded.workspace_id
			AND device_sessions.region_id=excluded.region_id
			AND device_sessions.device_id=excluded.device_id
			AND device_sessions.key_fingerprint=excluded.key_fingerprint
			AND device_sessions.credential_hash=excluded.credential_hash
			AND (device_sessions.revision < excluded.revision OR
		         (device_sessions.revision=excluded.revision AND device_sessions.expires_at=excluded.expires_at
				 AND device_sessions.authority_revision=excluded.authority_revision))`,
		permission.ID, permission.WorkspaceID, permission.RegionID, permission.DeviceID,
		permission.KeyFingerprint, permission.AuthorityRevision, permission.CredentialHash, permission.Revision, permission.ExpiresAt)
	if err == nil && tag.RowsAffected() == 0 {
		return ErrPairingFenced
	}
	return err
}

// RevokeDeviceSession creates a durable, terminal fence, even for an unknown session.
func (s *Store) RevokeDeviceSession(ctx context.Context, id string, revision int64) error {
	_, err := s.db.Exec(ctx, `
		INSERT INTO device_sessions (session_id,revision,revoked) VALUES ($1,$2,true)
		ON CONFLICT (session_id) DO UPDATE SET revision=GREATEST(device_sessions.revision,excluded.revision),revoked=true`, id, revision)
	return err
}

// DeviceSessionByCredential validates the credential without requiring a grant.
// A first-time Device must authenticate before its initial pairing.
func (s *Store) DeviceSessionByCredential(ctx context.Context, digest []byte, now time.Time) (DeviceSession, error) {
	return scanDeviceSession(s.db.QueryRow(ctx, `
		SELECT session_id,workspace_id,region_id,device_id,key_fingerprint,authority_revision,revision,expires_at
		FROM device_sessions WHERE credential_hash=$1 AND NOT revoked AND expires_at>$2`, digest, now))
}

// ActiveDeviceSession checks the installed permission for live use.
func (s *Store) ActiveDeviceSession(ctx context.Context, id string, now time.Time) (DeviceSession, error) {
	return scanDeviceSession(s.db.QueryRow(ctx, `
		SELECT session_id,workspace_id,region_id,device_id,key_fingerprint,authority_revision,revision,expires_at
		FROM device_sessions WHERE session_id=$1 AND NOT revoked AND expires_at>$2`, id, now))
}

func scanDeviceSession(row interface{ Scan(...any) error }) (DeviceSession, error) {
	var permission DeviceSession
	err := row.Scan(&permission.ID, &permission.WorkspaceID, &permission.RegionID, &permission.DeviceID,
		&permission.KeyFingerprint, &permission.AuthorityRevision, &permission.Revision, &permission.ExpiresAt)
	return permission, wrapRequestScan(err)
}

// DeviceSessionHosts restricts the Device's grants to the permit's workspace
// and the enrolled key. A Device ID alone never establishes session authority.
func (s *Store) DeviceSessionHosts(ctx context.Context, permission DeviceSession) (map[string]bool, error) {
	rows, err := s.poolQuery(ctx, `
		SELECT g.host_id FROM grant_authority g
		JOIN external_enrollments e ON e.host_id=g.host_id
		JOIN devices d ON d.device_id=g.device_id
		WHERE g.device_id=$1 AND d.public_key_fingerprint=$2 AND g.active
		AND e.workspace_id=$3 AND e.state='enrollment_ready' AND e.disabled_at IS NULL
		AND EXISTS (SELECT 1 FROM pairing_outcomes p
			WHERE p.host_id=g.host_id AND p.device_id=g.device_id AND p.grant_generation=g.grant_generation
			AND p.host_confirmed AND p.host_confirmation_verified AND p.commit_binding_verified
			AND octet_length(p.token_hash)=33)`, permission.DeviceID, permission.KeyFingerprint, permission.WorkspaceID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	hosts := make(map[string]bool)
	for rows.Next() {
		var host string
		if err := rows.Scan(&host); err != nil {
			return nil, err
		}
		hosts[host] = true
	}
	return hosts, rows.Err()
}

// DeviceSessionPairingStatus reports only a pairing bound to the current
// Device, workspace and requested Host. A missing outcome is distinct from a
// durable terminal fence, even when a renewal or response is in flight.
func (s *Store) DeviceSessionPairingStatus(ctx context.Context, permission DeviceSession, hostID, claimID string) (string, error) {
	var state string
	err := s.poolQueryRow(ctx, `
		SELECT CASE
			WHEN e.disabled_at IS NOT NULL OR e.state='enrollment_disabled'
				OR r.host_id IS NOT NULL OR g.revoked_at IS NOT NULL THEN 'terminal'
			WHEN o.host_confirmed AND o.host_confirmation_verified AND o.commit_binding_verified
				AND o.grant_generation=g.grant_generation AND g.revoked_at IS NULL THEN 'ready'
			WHEN p.expires_at <= clock_timestamp() OR prior.revoked
				OR p.authority_revision<>$6 OR prior.authority_revision<>p.authority_revision THEN 'terminal'
			ELSE 'pending' END
		FROM pairing_device_sessions p
		JOIN device_sessions prior ON prior.session_id=p.session_id
		JOIN external_enrollments e ON e.host_id=p.host_id AND e.workspace_id=p.workspace_id
		LEFT JOIN pairing_outcomes o ON o.claim_id=p.claim_id
		LEFT JOIN grants g ON g.host_id=p.host_id AND g.device_id=p.device_id
		LEFT JOIN external_device_revocations r ON r.host_id=p.host_id AND r.device_id=p.device_id
		WHERE p.claim_id=$1 AND p.host_id=$2 AND p.device_id=$3 AND p.workspace_id=$4
		AND prior.key_fingerprint=$5`, claimID, hostID, permission.DeviceID, permission.WorkspaceID,
		permission.KeyFingerprint, permission.AuthorityRevision).Scan(&state)
	return state, wrapRequestScan(err)
}

// lockActiveDeviceSession serializes admitting new work with explicit
// revocation. The timestamp is evaluated after PostgreSQL obtains the lock.
func lockActiveDeviceSession(ctx context.Context, tx pgx.Tx, id, deviceID, workspaceID, authority string) error {
	var active bool
	err := tx.QueryRow(ctx, `SELECT NOT revoked AND expires_at>clock_timestamp()
		FROM device_sessions WHERE session_id=$1 AND device_id=$2 AND ($3='' OR workspace_id=$3)
		AND ($4='' OR authority_revision=$4)
		FOR SHARE`, id, deviceID, workspaceID, authority).Scan(&active)
	if errors.Is(err, pgx.ErrNoRows) || (err == nil && !active) {
		return ErrDeviceSessionEnded
	}
	return err
}

// ErrDeviceSessionEnded means the request's original permission is inactive.
var ErrDeviceSessionEnded = errors.New("device session ended")

// PublishDuringDeviceSession serializes forwarding with explicit revocation.
// Standalone and legacy rows have no session scope.
func (s *Store) PublishDuringDeviceSession(ctx context.Context, requestID string, publish func() error) error {
	var sessionID, authority *string
	var hostID, deviceID string
	var kind int
	err := s.poolQueryRow(ctx, `SELECT device_session_id,device_session_authority,host_id,device_id,kind
		FROM requests WHERE request_id=$1`, requestID).Scan(&sessionID, &authority, &hostID, &deviceID, &kind)
	if err != nil {
		return wrapRequestScan(err)
	}
	if sessionID == nil {
		return publish()
	}
	current := servingDeviceSessionAuthority(ctx)
	if authority == nil || (current != "" && *authority != current) {
		return ErrDeviceSessionEnded
	}
	tx, err := s.poolBegin(ctx)
	if err != nil {
		return err
	}
	defer func() { _ = tx.Rollback(ctx) }()
	if err := lockActiveDeviceSession(ctx, tx, *sessionID, deviceID, "", *authority); err != nil {
		return err
	}
	if kind != int(whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE) {
		var granted bool
		err := tx.QueryRow(ctx, `SELECT EXISTS (
			SELECT 1 FROM grant_authority g JOIN external_enrollments e ON e.host_id=g.host_id
			JOIN device_sessions s ON s.workspace_id=e.workspace_id
			WHERE s.session_id=$1 AND g.device_id=$2 AND g.host_id=$3 AND g.active
			AND e.state='enrollment_ready' AND e.disabled_at IS NULL)`, *sessionID, deviceID, hostID).Scan(&granted)
		if err != nil {
			return err
		}
		if !granted {
			return ErrDeviceSessionEnded
		}
	}
	return publish()
}

// SessionCancellationReason distinguishes explicit or elapsed session authority
// from client poll inactivity while retaining the same exact journal identity.
func (s *Store) SessionCancellationReason(ctx context.Context, requestID string) (bool, error) {
	var ended bool
	err := s.poolQueryRow(ctx, `SELECT EXISTS(
		SELECT 1 FROM requests r JOIN device_sessions s ON s.session_id=r.device_session_id
		WHERE r.request_id=$1 AND (s.revoked OR s.expires_at<=clock_timestamp()
			OR s.authority_revision IS DISTINCT FROM r.device_session_authority
			OR ($2<>'' AND s.authority_revision<>$2)))`, requestID, servingDeviceSessionAuthority(ctx)).Scan(&ended)
	return ended, err
}

type requestSessionKey struct{}

// WithRequestDeviceSession binds lease extensions to the caller's session.
func WithRequestDeviceSession(ctx context.Context, id string) context.Context {
	if id == "" {
		return ctx
	}
	return context.WithValue(ctx, requestSessionKey{}, id)
}

type sessionAuthorityKey struct{}

// WithDeviceSessionAuthority records this server's serving authority for
// queued forwarding, confirmation and lease reconciliation.
func WithDeviceSessionAuthority(ctx context.Context, revision string) context.Context {
	if revision == "" {
		return ctx
	}
	return context.WithValue(ctx, sessionAuthorityKey{}, revision)
}

func servingDeviceSessionAuthority(ctx context.Context) string {
	authority, _ := ctx.Value(sessionAuthorityKey{}).(string)
	return authority
}

func applyDeviceSessionAuthority(ctx context.Context, tx pgx.Tx) error {
	authority := servingDeviceSessionAuthority(ctx)
	if authority == "" {
		return nil
	}
	_, err := tx.Exec(ctx, `SELECT set_config('whalerein.session_authority',$1,true)`, authority)
	return err
}
