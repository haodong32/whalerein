package store

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"errors"
	"fmt"
	"time"

	"github.com/jackc/pgx/v5"
)

// ErrOperationConflict reports the same authorization used for another payload.
var ErrOperationConflict = errors.New("operation payload does not match the recorded intent")

// ExternalOperation is one durable enrollment intent.
type ExternalOperation struct {
	OperationID        string
	AuthorizationID    string
	Purpose            string
	WorkspaceID        string
	RegionID           string
	HostID             string
	PayloadDigest      []byte
	State              string
	EnrollmentRevision string
	CommittedAt        *time.Time
}

// ExternalEnrollment is one approved Host binding.
type ExternalEnrollment struct {
	HostID               string
	WorkspaceID          string
	EnrollmentRevision   string
	HostSigningPublicKey []byte
	HostNoisePublicKey   []byte
	OperationID          string
	State                string
	DisabledAt           *time.Time
}

// EnrollmentCredential is the broker admission record for one enrolled Host.
// Only the hash is stored; plaintext never enters this row.
type EnrollmentCredential struct {
	HostID               string
	BrokerCredentialHash []byte
	State                string
	DisabledAt           *time.Time
}

// DisabledEnrollment is the outcome of one durable Host disable. Revocations
// maps each device whose grant this call revoked to its grant generation, so
// the line can deny exactly the current incarnation.
type DisabledEnrollment struct {
	HostID      string
	DisabledAt  time.Time
	Revocations map[string]uint64
}

// EnrollHost records one Host enrollment.
//
// The operation insert, the enrollment insert and the credential hash commit
// together, so a retry after a lost reply either returns the recorded result
// or reports a conflict. It returns the plaintext credential only when this
// call committed the enrollment.
func (s *Store) EnrollHost(
	ctx context.Context,
	operation ExternalOperation,
	enrollment ExternalEnrollment,
) (ExternalEnrollment, string, bool, error) {
	tx, err := s.db.Begin(ctx)
	if err != nil {
		return ExternalEnrollment{}, "", false, fmt.Errorf("begin: %w", err)
	}
	defer func() { _ = tx.Rollback(ctx) }()

	// The unique authorization id binds one intent. A concurrent duplicate
	// waits here and then observes the recorded row.
	var inserted string
	err = tx.QueryRow(ctx, `
		INSERT INTO external_operations
			(operation_id, authorization_id, purpose, workspace_id, region_id, host_id,
			 payload_digest, state, enrollment_revision, committed_at)
		VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, now())
		ON CONFLICT (authorization_id) DO NOTHING
		RETURNING operation_id`,
		operation.OperationID, operation.AuthorizationID, operation.Purpose,
		operation.WorkspaceID, operation.RegionID, operation.HostID,
		operation.PayloadDigest, operation.State, operation.EnrollmentRevision).
		Scan(&inserted)
	if errors.Is(err, pgx.ErrNoRows) {
		// The intent already exists: return the recorded outcome when the
		// payload is identical, otherwise refuse.
		recorded, loadErr := s.operationByAuthorization(ctx, tx, operation.AuthorizationID)
		if loadErr != nil {
			return ExternalEnrollment{}, "", false, loadErr
		}
		if string(recorded.PayloadDigest) != string(operation.PayloadDigest) {
			return ExternalEnrollment{}, "", false, ErrOperationConflict
		}
		if err := tx.Commit(ctx); err != nil {
			return ExternalEnrollment{}, "", false, fmt.Errorf("commit: %w", err)
		}
		existing, loadErr := s.Enrollment(ctx, operation.HostID)
		if loadErr != nil {
			return ExternalEnrollment{}, "", false, loadErr
		}
		return existing, "", false, nil
	}
	if err != nil {
		return ExternalEnrollment{}, "", false, fmt.Errorf("record operation: %w", err)
	}

	credential, err := newCredential()
	if err != nil {
		return ExternalEnrollment{}, "", false, err
	}
	_, err = tx.Exec(ctx, `
		INSERT INTO external_enrollments
			(host_id, workspace_id, enrollment_revision, host_signing_public_key,
			 host_noise_public_key, broker_credential_hash, operation_id, state)
		VALUES ($1, $2, $3, $4, $5, $6, $7, $8)`,
		enrollment.HostID, enrollment.WorkspaceID, enrollment.EnrollmentRevision,
		enrollment.HostSigningPublicKey, enrollment.HostNoisePublicKey,
		hashCredential(credential), operation.OperationID, enrollment.State)
	if err != nil {
		if isUniqueViolation(err) {
			// A second enrollment for the same Host is a conflict, not a
			// primary-key failure. Re-adding requires a new authorized
			// revision through an explicit flow.
			return ExternalEnrollment{}, "", false, ErrConflict
		}
		return ExternalEnrollment{}, "", false, fmt.Errorf("record enrollment: %w", err)
	}
	if err := tx.Commit(ctx); err != nil {
		return ExternalEnrollment{}, "", false, fmt.Errorf("commit: %w", err)
	}
	enrollment.OperationID = operation.OperationID
	enrollment.State = "enrollment_pending"
	return enrollment, credential, true, nil
}

// Enrollment loads one Host enrollment.
func (s *Store) Enrollment(ctx context.Context, hostID string) (ExternalEnrollment, error) {
	return loadEnrollment(ctx, s.db, hostID)
}

const enrollmentColumns = `host_id, workspace_id, enrollment_revision, host_signing_public_key,
	       host_noise_public_key, operation_id, state, disabled_at`

func loadEnrollment(ctx context.Context, q querier, hostID string) (ExternalEnrollment, error) {
	var enrollment ExternalEnrollment
	err := q.QueryRow(ctx, `SELECT `+enrollmentColumns+`
		FROM external_enrollments WHERE host_id = $1`, hostID).Scan(
		&enrollment.HostID, &enrollment.WorkspaceID, &enrollment.EnrollmentRevision,
		&enrollment.HostSigningPublicKey, &enrollment.HostNoisePublicKey,
		&enrollment.OperationID, &enrollment.State, &enrollment.DisabledAt)
	if errors.Is(err, pgx.ErrNoRows) {
		return ExternalEnrollment{}, ErrNotFound
	}
	if err != nil {
		return ExternalEnrollment{}, fmt.Errorf("load enrollment: %w", err)
	}
	return enrollment, nil
}

// HostBrokerCredential loads the credential hash and lifecycle state for
// broker admission. It never loads plaintext and returns ErrNotFound when no
// external enrollment exists for the Host.
func (s *Store) HostBrokerCredential(ctx context.Context, hostID string) (EnrollmentCredential, error) {
	var credential EnrollmentCredential
	err := s.db.QueryRow(ctx, `
		SELECT host_id, broker_credential_hash, state, disabled_at
		FROM external_enrollments WHERE host_id = $1`, hostID).Scan(
		&credential.HostID, &credential.BrokerCredentialHash,
		&credential.State, &credential.DisabledAt)
	if errors.Is(err, pgx.ErrNoRows) {
		return EnrollmentCredential{}, ErrNotFound
	}
	if err != nil {
		return EnrollmentCredential{}, fmt.Errorf("load host broker credential: %w", err)
	}
	return credential, nil
}

// MarkEnrollmentReady advances one pending enrollment to ready and records the
// operation outcome in one transaction. It is idempotent: an already-ready,
// disabled or unknown enrollment reports advanced=false and the current row.
func (s *Store) MarkEnrollmentReady(ctx context.Context, hostID string, at time.Time) (ExternalEnrollment, bool, error) {
	if at.IsZero() {
		at = time.Now().UTC()
	}
	tx, err := s.db.Begin(ctx)
	if err != nil {
		return ExternalEnrollment{}, false, fmt.Errorf("begin enrollment ready: %w", err)
	}
	defer func() { _ = tx.Rollback(ctx) }()

	var enrollment ExternalEnrollment
	err = tx.QueryRow(ctx, `UPDATE external_enrollments SET state = 'enrollment_ready'
		WHERE host_id = $1 AND state = 'enrollment_pending' AND disabled_at IS NULL
		RETURNING `+enrollmentColumns, hostID).Scan(
		&enrollment.HostID, &enrollment.WorkspaceID, &enrollment.EnrollmentRevision,
		&enrollment.HostSigningPublicKey, &enrollment.HostNoisePublicKey,
		&enrollment.OperationID, &enrollment.State, &enrollment.DisabledAt)
	if errors.Is(err, pgx.ErrNoRows) {
		// A HostReady for an unknown host is a standalone Host or already
		// handled; return the current row when one exists and never create one.
		existing, loadErr := loadEnrollment(ctx, tx, hostID)
		if loadErr != nil {
			return ExternalEnrollment{}, false, loadErr
		}
		if err := tx.Commit(ctx); err != nil {
			return ExternalEnrollment{}, false, fmt.Errorf("commit enrollment ready: %w", err)
		}
		return existing, false, nil
	}
	if err != nil {
		return ExternalEnrollment{}, false, fmt.Errorf("advance enrollment ready: %w", err)
	}
	if _, err := tx.Exec(ctx, `
		UPDATE external_operations SET outcome = 'enrollment_ready', updated_at = $2
		WHERE operation_id = $1`, enrollment.OperationID, at); err != nil {
		return ExternalEnrollment{}, false, fmt.Errorf("record enrollment readiness: %w", err)
	}
	if err := tx.Commit(ctx); err != nil {
		return ExternalEnrollment{}, false, fmt.Errorf("commit enrollment ready: %w", err)
	}
	return enrollment, true, nil
}

// DisableEnrollment durably disables one enrollment and revokes every live
// grant for that Host in one transaction. It is idempotent: a repeated call
// returns the original disabled_at and an empty revocation set.
func (s *Store) DisableEnrollment(ctx context.Context, hostID string, at time.Time) (DisabledEnrollment, error) {
	if at.IsZero() {
		at = time.Now().UTC()
	}
	tx, err := s.db.Begin(ctx)
	if err != nil {
		return DisabledEnrollment{}, fmt.Errorf("begin disable enrollment: %w", err)
	}
	defer func() { _ = tx.Rollback(ctx) }()

	// Serialize concurrent disable and enrollment/recovery for this Host.
	if _, err := tx.Exec(ctx, `SELECT pg_advisory_xact_lock(hashtextextended('whalerein-host:' || $1, 0))`, hostID); err != nil {
		return DisabledEnrollment{}, fmt.Errorf("lock host enrollment: %w", err)
	}
	var operationID string
	var disabledAt *time.Time
	err = tx.QueryRow(ctx, `
		SELECT operation_id, disabled_at FROM external_enrollments
		WHERE host_id = $1 FOR UPDATE`, hostID).Scan(&operationID, &disabledAt)
	if errors.Is(err, pgx.ErrNoRows) {
		return DisabledEnrollment{}, ErrNotFound
	}
	if err != nil {
		return DisabledEnrollment{}, fmt.Errorf("load enrollment for disable: %w", err)
	}
	if disabledAt == nil {
		if err := tx.QueryRow(ctx, `
			UPDATE external_enrollments
			SET disabled_at = $2, state = 'enrollment_disabled'
			WHERE host_id = $1
			RETURNING disabled_at`, hostID, at).Scan(&disabledAt); err != nil {
			return DisabledEnrollment{}, fmt.Errorf("disable enrollment: %w", err)
		}
	}

	rows, err := tx.Query(ctx, `
		UPDATE grants SET revoked_at = COALESCE(revoked_at, $2), updated_at = $2,
		       revocation_generation = revocation_generation + 1
		WHERE host_id = $1 AND revoked_at IS NULL
		RETURNING device_id, grant_generation`, hostID, at)
	if err != nil {
		return DisabledEnrollment{}, fmt.Errorf("revoke disabled host grants: %w", err)
	}
	revocations := map[string]uint64{}
	deviceIDs := make([]string, 0)
	for rows.Next() {
		var deviceID string
		var generation uint64
		if err := rows.Scan(&deviceID, &generation); err != nil {
			rows.Close()
			return DisabledEnrollment{}, fmt.Errorf("scan disabled host grant: %w", err)
		}
		revocations[deviceID] = generation
		deviceIDs = append(deviceIDs, deviceID)
	}
	rows.Close()
	if err := rows.Err(); err != nil {
		return DisabledEnrollment{}, fmt.Errorf("revoke disabled host grants: %w", err)
	}
	if len(deviceIDs) > 0 {
		// A device with no live grant must not keep a usable transport
		// credential, matching the ordinary revoke path.
		if _, err := tx.Exec(ctx, `
			DELETE FROM device_credentials dc
			WHERE dc.device_id = ANY($1)
			  AND NOT EXISTS (
				SELECT 1 FROM grants g
				WHERE g.device_id = dc.device_id AND g.revoked_at IS NULL)`, deviceIDs); err != nil {
			return DisabledEnrollment{}, fmt.Errorf("delete disabled host credentials: %w", err)
		}
	}
	if _, err := tx.Exec(ctx, `
		UPDATE external_operations SET outcome = 'enrollment_disabled', updated_at = $2
		WHERE operation_id = $1`, operationID, at); err != nil {
		return DisabledEnrollment{}, fmt.Errorf("record enrollment disable: %w", err)
	}
	if err := tx.Commit(ctx); err != nil {
		return DisabledEnrollment{}, fmt.Errorf("commit disable enrollment: %w", err)
	}
	return DisabledEnrollment{HostID: hostID, DisabledAt: *disabledAt, Revocations: revocations}, nil
}

// ReplaceHostCredential atomically records one credential-replacement
// operation and replaces the enrollment credential hash. The plaintext is
// returned only by the call that commits; a replay of the same authorization
// id conflicts and never returns a credential.
func (s *Store) ReplaceHostCredential(ctx context.Context, operation ExternalOperation) (ExternalEnrollment, string, error) {
	tx, err := s.db.Begin(ctx)
	if err != nil {
		return ExternalEnrollment{}, "", fmt.Errorf("begin credential replacement: %w", err)
	}
	defer func() { _ = tx.Rollback(ctx) }()

	var inserted string
	err = tx.QueryRow(ctx, `
		INSERT INTO external_operations
			(operation_id, authorization_id, purpose, workspace_id, region_id, host_id,
			 payload_digest, state, enrollment_revision, committed_at)
		VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, now())
		ON CONFLICT (authorization_id) DO NOTHING
		RETURNING operation_id`,
		operation.OperationID, operation.AuthorizationID, operation.Purpose,
		operation.WorkspaceID, operation.RegionID, operation.HostID,
		operation.PayloadDigest, operation.State, operation.EnrollmentRevision).
		Scan(&inserted)
	if errors.Is(err, pgx.ErrNoRows) {
		// A fresh replacement authorization is single-use. A replay never
		// reveals whether the original call committed a credential.
		return ExternalEnrollment{}, "", ErrOperationConflict
	}
	if err != nil {
		if isUniqueViolation(err) {
			return ExternalEnrollment{}, "", ErrOperationConflict
		}
		return ExternalEnrollment{}, "", fmt.Errorf("record credential replacement: %w", err)
	}

	credential, err := newCredential()
	if err != nil {
		return ExternalEnrollment{}, "", err
	}
	var enrollment ExternalEnrollment
	err = tx.QueryRow(ctx, `
		UPDATE external_enrollments
		SET broker_credential_hash = $2
		WHERE host_id = $1 AND workspace_id = $3 AND enrollment_revision = $4
		  AND disabled_at IS NULL AND state <> 'enrollment_disabled'
		RETURNING `+enrollmentColumns,
		operation.HostID, hashCredential(credential), operation.WorkspaceID,
		operation.EnrollmentRevision).Scan(
		&enrollment.HostID, &enrollment.WorkspaceID, &enrollment.EnrollmentRevision,
		&enrollment.HostSigningPublicKey, &enrollment.HostNoisePublicKey,
		&enrollment.OperationID, &enrollment.State, &enrollment.DisabledAt)
	if errors.Is(err, pgx.ErrNoRows) {
		// The enrollment was disabled or its revision changed after the
		// pre-check. Roll back so the authorization id is not consumed, but
		// distinguish a genuinely missing enrollment from a conflict.
		if _, loadErr := loadEnrollment(ctx, tx, operation.HostID); loadErr != nil {
			return ExternalEnrollment{}, "", loadErr
		}
		return ExternalEnrollment{}, "", ErrConflict
	}
	if err != nil {
		return ExternalEnrollment{}, "", fmt.Errorf("replace host credential: %w", err)
	}
	if _, err := tx.Exec(ctx, `
		UPDATE external_operations
		SET state = 'operation_committed', committed_at = COALESCE(committed_at, now()),
		    outcome = 'credential_replaced', updated_at = now()
		WHERE operation_id = $1`, operation.OperationID); err != nil {
		return ExternalEnrollment{}, "", fmt.Errorf("record credential replacement outcome: %w", err)
	}
	if err := tx.Commit(ctx); err != nil {
		return ExternalEnrollment{}, "", fmt.Errorf("commit credential replacement: %w", err)
	}
	return enrollment, credential, nil
}

// Operation loads one operation record.
func (s *Store) Operation(ctx context.Context, operationID string) (ExternalOperation, error) {
	operation, err := s.operationByAuthorization(ctx, s.db, operationID)
	if errors.Is(err, ErrNotFound) {
		return ExternalOperation{}, ErrNotFound
	}
	return operation, err
}

// querier is the shared subset of pgxpool.Pool and pgx.Tx.
type querier interface {
	QueryRow(ctx context.Context, sql string, args ...any) pgx.Row
}

func (s *Store) operationByAuthorization(ctx context.Context, q querier, key string) (ExternalOperation, error) {
	// The parameter matches either identifier: an operation id is a
	// prefix-free opaque handle, so one statement serves both lookups.
	var operation ExternalOperation
	err := q.QueryRow(ctx, `
		SELECT operation_id, authorization_id, purpose, workspace_id, region_id,
		       host_id, payload_digest, state, enrollment_revision, committed_at
		FROM external_operations
		WHERE operation_id = $1 OR authorization_id = $1`, key).Scan(
		&operation.OperationID, &operation.AuthorizationID, &operation.Purpose,
		&operation.WorkspaceID, &operation.RegionID, &operation.HostID,
		&operation.PayloadDigest, &operation.State, &operation.EnrollmentRevision,
		&operation.CommittedAt)
	if errors.Is(err, pgx.ErrNoRows) {
		return ExternalOperation{}, ErrNotFound
	}
	if err != nil {
		return ExternalOperation{}, fmt.Errorf("load operation: %w", err)
	}
	return operation, nil
}

// NextEnrollmentRevision returns the next revision for a workspace.
func (s *Store) NextEnrollmentRevision(ctx context.Context, workspaceID string) (string, error) {
	var count int
	if err := s.db.QueryRow(ctx,
		`SELECT count(*) FROM external_enrollments WHERE workspace_id = $1`,
		workspaceID).Scan(&count); err != nil {
		return "", fmt.Errorf("count enrollments: %w", err)
	}
	return fmt.Sprintf("%d", count+1), nil
}

func newCredential() (string, error) {
	raw := make([]byte, 32)
	if _, err := rand.Read(raw); err != nil {
		return "", fmt.Errorf("generate credential: %w", err)
	}
	return base64.RawURLEncoding.EncodeToString(raw), nil
}

func hashCredential(credential string) []byte {
	sum := sha256.Sum256([]byte(credential))
	return sum[:]
}

// NewOperationID returns a random operation id.
func NewOperationID() string {
	raw := make([]byte, 12)
	if _, err := rand.Read(raw); err != nil {
		panic(fmt.Sprintf("generate operation id: %v", err))
	}
	return "op-" + hex.EncodeToString(raw)
}
