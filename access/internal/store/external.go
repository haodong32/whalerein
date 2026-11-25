package store

import (
	"bytes"
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"strconv"
	"time"

	"github.com/jackc/pgx/v5"
)

// External operation outcomes. A mutation that rolls back leaves the admitted
// row visible; an operation is never reported committed without a real commit.
const (
	OperationAdmitted  = "admitted"
	OperationCommitted = "committed"
	OperationRefused   = "refused"
	OperationExpired   = "expired"
	OperationFenced    = "fenced"
	OperationAbsent    = "absent"
	OperationPending   = "pending"
)

// Stable external operation refusals.
var (
	// ErrOperationConflict reports the same authorization used for another payload.
	ErrOperationConflict = errors.New("operation payload does not match the recorded intent")
	// ErrAuthorizationExpired proves the admitted authorization deadline passed.
	ErrAuthorizationExpired = errors.New("authorization expired")
	// ErrOperationFenced proves a rolled-back or superseded attempt cannot mutate.
	ErrOperationFenced = errors.New("operation is fenced and cannot commit")
	// ErrOperationPending reports an admitted operation whose mutation is unresolved.
	ErrOperationPending = errors.New("operation is still pending")
	// ErrOperationRefused reports a recorded refusal.
	ErrOperationRefused = errors.New("operation was refused")
	// ErrDuplicateHost reports a proven duplicate registration attempt.
	ErrDuplicateHost = errors.New("host is already enrolled")
	// ErrEnrollmentDisabled reports a disabled binding.
	ErrEnrollmentDisabled = errors.New("enrollment is disabled")
	// ErrTargetMismatch reports a removal target that is not the effective binding.
	ErrTargetMismatch = errors.New("removal target does not match the effective enrollment")
)

// ExternalOperation is one durable operation intent and its recorded outcome.
type ExternalOperation struct {
	OperationID                string
	AuthorizationID            string
	Purpose                    string
	WorkspaceID                string
	RegionID                   string
	HostID                     string
	PayloadDigest              []byte
	State                      string
	EnrollmentRevision         string
	CommittedAt                *time.Time
	AuthorizationExpiresAt     *time.Time
	ExpectedWorkspaceID        string
	ExpectedEnrollmentRevision string
	AdmittedAt                 *time.Time
	Outcome                    string
	Reason                     string
	UpdatedAt                  *time.Time
	CredentialIssued           bool
	CredentialGeneration       uint64
	CredentialOperationID      string
}

// ExternalEnrollment is one approved Host binding with its credential and
// removal lifecycle. RemovalState is derived from the latest removal row.
type ExternalEnrollment struct {
	HostID                       string
	WorkspaceID                  string
	EnrollmentRevision           string
	Generation                   int64
	HostSigningPublicKey         []byte
	HostNoisePublicKey           []byte
	OperationID                  string
	State                        string
	DisabledAt                   *time.Time
	CurrentCredentialOperationID string
	CredentialGeneration         uint64
	RemovalState                 string
	RemovalOperationID           string
	RemovalEnforcedAt            *time.Time
}

// EnrollmentCredential is the broker admission record for one enrolled Host.
// Only the hash is stored; plaintext never enters this row.
type EnrollmentCredential struct {
	HostID               string
	BrokerCredentialHash []byte
	State                string
	DisabledAt           *time.Time
}

// RemovalTarget is the immutable binding a removal is allowed to disable.
type RemovalTarget struct {
	WorkspaceID           string
	EnrollmentOperationID string
	EnrollmentRevision    string
}

// matched reports whether a stored engagement is exactly this target.
func (t RemovalTarget) matched(other RemovalTarget) bool {
	return t.WorkspaceID == other.WorkspaceID &&
		t.EnrollmentOperationID == other.EnrollmentOperationID &&
		t.EnrollmentRevision == other.EnrollmentRevision
}

// DisabledEnrollment is the durable outcome of one Host removal. Revocations
// maps each device whose grant this removal revoked to its grant generation, so
// a retry can re-issue exactly the stored RevokeHost denials.
type DisabledEnrollment struct {
	HostID             string
	RemovalOperationID string
	Target             RemovalTarget
	// Generation is the frozen enrollment generation of the removal.
	Generation  uint64
	DisabledAt  time.Time
	EnforcedAt  *time.Time
	Revocations map[string]uint64
}

// removalRow is the stored removal record.
type removalRow struct {
	RemovalOperationID    string
	HostID                string
	WorkspaceID           string
	EnrollmentOperationID string
	EnrollmentRevision    string
	Generation            int64
	DisabledAt            time.Time
	EnforcedAt            *time.Time
	Revocations           map[string]uint64
}

// EnrollHost records one Host enrollment.
//
// The operation must already have been admitted, or this call admits it in its
// own committed transaction. The mutation then takes the Host and operation
// advisory locks, re-reads the admission, re-checks the authorization deadline
// with clock_timestamp(), and only then inserts the enrollment and the
// credential hash. The plaintext credential is returned only when this call
// committed the enrollment.
func (s *Store) EnrollHost(
	ctx context.Context,
	operation ExternalOperation,
	enrollment ExternalEnrollment,
) (ExternalEnrollment, string, bool, error) {
	if operation.OperationID == "" {
		return ExternalEnrollment{}, "", false, errors.New("store: operation id is required")
	}
	if err := s.ensureAdmission(ctx, operation); err != nil {
		return ExternalEnrollment{}, "", false, err
	}
	tx, err := s.db.Begin(ctx)
	if err != nil {
		return ExternalEnrollment{}, "", false, fmt.Errorf("begin: %w", err)
	}
	defer func() { _ = tx.Rollback(ctx) }()

	if err := lockHostScope(ctx, tx, enrollment.HostID); err != nil {
		return ExternalEnrollment{}, "", false, err
	}
	admitted, err := loadOperationForUpdate(ctx, tx, operation.OperationID)
	if err != nil {
		return ExternalEnrollment{}, "", false, err
	}
	switch admitted.Outcome {
	case OperationCommitted:
		existing, loadErr := loadEnrollment(ctx, tx, enrollment.HostID)
		if loadErr != nil {
			return ExternalEnrollment{}, "", false, loadErr
		}
		if existing.OperationID != admitted.OperationID {
			// A replay of an old generation's authorization must never return
			// the current generation's status or credential.
			return ExternalEnrollment{}, "", false, ErrOperationConflict
		}
		if err := tx.Commit(ctx); err != nil {
			return ExternalEnrollment{}, "", false, fmt.Errorf("commit: %w", err)
		}
		return existing, "", false, nil
	case OperationRefused, OperationExpired, OperationFenced:
		return ExternalEnrollment{}, "", false, refusedOperationError(admitted)
	}
	if !bytes.Equal(admitted.PayloadDigest, operation.PayloadDigest) {
		return ExternalEnrollment{}, "", false, ErrOperationConflict
	}
	expired, err := operationAdmissionExpired(ctx, tx, admitted)
	if err != nil {
		return ExternalEnrollment{}, "", false, err
	}
	if expired {
		if err := markOperationOutcome(ctx, tx, admitted.OperationID, OperationExpired, "authorization_expired"); err != nil {
			return ExternalEnrollment{}, "", false, err
		}
		if err := tx.Commit(ctx); err != nil {
			return ExternalEnrollment{}, "", false, fmt.Errorf("commit expired operation: %w", err)
		}
		return ExternalEnrollment{}, "", false, ErrAuthorizationExpired
	}

	credential, err := newCredential()
	if err != nil {
		return ExternalEnrollment{}, "", false, err
	}
	var recorded ExternalEnrollment
	// A duplicate Host is detected without aborting the transaction so the
	// refusal journal row can still commit.
	err = scanEnrollment(tx.QueryRow(ctx, `
		INSERT INTO external_enrollments
			(host_id, workspace_id, enrollment_revision, generation, host_signing_public_key,
			 host_noise_public_key, broker_credential_hash, operation_id, state,
			 current_credential_operation_id, credential_generation)
		VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'enrollment_pending', $8, 1)
		ON CONFLICT (host_id) DO NOTHING
		RETURNING `+enrollmentColumns,
		enrollment.HostID, admitted.WorkspaceID, admitted.EnrollmentRevision,
		revisionGeneration(admitted.EnrollmentRevision), enrollment.HostSigningPublicKey, enrollment.HostNoisePublicKey,
		hashCredential(credential), admitted.OperationID), &recorded)
	if errors.Is(err, pgx.ErrNoRows) {
		existing, loadErr := loadEnrollment(ctx, tx, enrollment.HostID)
		if loadErr != nil {
			return ExternalEnrollment{}, "", false, loadErr
		}
		if existing.DisabledAt == nil && existing.State != "enrollment_disabled" {
			if err := markOperationOutcome(ctx, tx, admitted.OperationID, OperationRefused, "duplicate_host"); err != nil {
				return ExternalEnrollment{}, "", false, err
			}
			if err := tx.Commit(ctx); err != nil {
				return ExternalEnrollment{}, "", false, fmt.Errorf("commit refusal: %w", err)
			}
			return ExternalEnrollment{}, "", false, ErrDuplicateHost
		}
		// A removed enrollment is terminal for its generation, not for the
		// Host identity. A fresh Cloud authorization with a strictly higher
		// revision starts the next generation and re-enables the same Host id.
		// The workspace and both Host keys must be identical: a different key
		// for a leaked host id is a takeover, not a re-enable.
		if existing.WorkspaceID != admitted.WorkspaceID ||
			!bytes.Equal(existing.HostSigningPublicKey, enrollment.HostSigningPublicKey) ||
			!bytes.Equal(existing.HostNoisePublicKey, enrollment.HostNoisePublicKey) ||
			!higherRevision(admitted.EnrollmentRevision, existing.EnrollmentRevision) {
			if err := markOperationOutcome(ctx, tx, admitted.OperationID, OperationRefused, "enrollment_disabled"); err != nil {
				return ExternalEnrollment{}, "", false, err
			}
			if err := tx.Commit(ctx); err != nil {
				return ExternalEnrollment{}, "", false, fmt.Errorf("commit refusal: %w", err)
			}
			return ExternalEnrollment{}, "", false, ErrEnrollmentDisabled
		}
		err = scanEnrollment(tx.QueryRow(ctx, `
			UPDATE external_enrollments
			SET enrollment_revision = $2,
			    generation = $3,
			    host_signing_public_key = $4,
			    host_noise_public_key = $5,
			    broker_credential_hash = $6,
			    operation_id = $7,
			    state = 'enrollment_pending',
			    disabled_at = NULL,
			    current_credential_operation_id = $7,
			    credential_generation = credential_generation + 1
			WHERE host_id = $1
			RETURNING `+enrollmentColumns,
			enrollment.HostID, admitted.EnrollmentRevision, revisionGeneration(admitted.EnrollmentRevision),
			enrollment.HostSigningPublicKey, enrollment.HostNoisePublicKey,
			hashCredential(credential), admitted.OperationID), &recorded)
		if err != nil {
			return ExternalEnrollment{}, "", false, fmt.Errorf("re-enable enrollment: %w", err)
		}
	}
	if err != nil {
		return ExternalEnrollment{}, "", false, fmt.Errorf("record enrollment: %w", err)
	}
	if _, err := tx.Exec(ctx, `
		UPDATE external_operations
		SET state = 'operation_committed', outcome = 'committed', reason = '',
		    committed_at = clock_timestamp(), updated_at = clock_timestamp(),
		    credential_issued = true, credential_generation = $2,
		    credential_operation_id = $1
		WHERE operation_id = $1`, admitted.OperationID, recorded.CredentialGeneration); err != nil {
		return ExternalEnrollment{}, "", false, fmt.Errorf("record enrollment outcome: %w", err)
	}
	if err := tx.Commit(ctx); err != nil {
		return ExternalEnrollment{}, "", false, fmt.Errorf("commit: %w", err)
	}
	return recorded, credential, true, nil
}

// ensureAdmission records the operation if it has not been admitted yet. The
// service admits explicitly before calling a mutation; this fallback keeps a
// direct store caller on the same admission-before-mutation path.
func (s *Store) ensureAdmission(ctx context.Context, operation ExternalOperation) error {
	if _, err := loadOperationByID(ctx, s.db, operation.OperationID); err == nil {
		return nil
	} else if !errors.Is(err, ErrNotFound) {
		return err
	}
	_, _, err := s.AdmitOperation(ctx, operation)
	return err
}

// AdmitOperation records one operation in its own committed transaction before
// any mutation. Replaying the same payload returns the recorded row and
// admitted=false; a changed payload is a conflict and creates no row.
func (s *Store) AdmitOperation(ctx context.Context, operation ExternalOperation) (ExternalOperation, bool, error) {
	if operation.OperationID == "" {
		return ExternalOperation{}, false, errors.New("store: operation id is required")
	}
	tx, err := s.db.Begin(ctx)
	if err != nil {
		return ExternalOperation{}, false, fmt.Errorf("begin admission: %w", err)
	}
	defer func() { _ = tx.Rollback(ctx) }()

	var inserted string
	err = tx.QueryRow(ctx, `
		INSERT INTO external_operations
			(operation_id, authorization_id, purpose, workspace_id, region_id, host_id,
			 payload_digest, state, enrollment_revision, expected_workspace_id,
			 expected_enrollment_revision, authorization_expires_at, outcome,
			 admitted_at, updated_at)
		VALUES ($1, $2, $3, $4, $5, $6, $7, 'operation_admitted', $8, $9, $10, $11,
		        'admitted', clock_timestamp(), clock_timestamp())
		ON CONFLICT (authorization_id) DO NOTHING
		RETURNING operation_id`,
		operation.OperationID, operation.AuthorizationID, operation.Purpose,
		operation.WorkspaceID, operation.RegionID, operation.HostID,
		operation.PayloadDigest, operation.EnrollmentRevision,
		operation.ExpectedWorkspaceID, operation.ExpectedEnrollmentRevision,
		nullableTime(operation.AuthorizationExpiresAt)).Scan(&inserted)
	if errors.Is(err, pgx.ErrNoRows) {
		recorded, loadErr := loadOperationByAuthorization(ctx, tx, operation.AuthorizationID)
		if loadErr != nil {
			return ExternalOperation{}, false, loadErr
		}
		if !admissionMatches(recorded, operation) {
			return ExternalOperation{}, false, ErrOperationConflict
		}
		if err := tx.Commit(ctx); err != nil {
			return ExternalOperation{}, false, fmt.Errorf("commit admission replay: %w", err)
		}
		return recorded, false, nil
	}
	if err != nil {
		if isUniqueViolation(err) {
			recorded, loadErr := loadOperationByID(ctx, tx, operation.OperationID)
			if loadErr != nil {
				return ExternalOperation{}, false, ErrOperationConflict
			}
			if !admissionMatches(recorded, operation) {
				return ExternalOperation{}, false, ErrOperationConflict
			}
			if err := tx.Commit(ctx); err != nil {
				return ExternalOperation{}, false, fmt.Errorf("commit admission replay: %w", err)
			}
			return recorded, false, nil
		}
		return ExternalOperation{}, false, fmt.Errorf("record admission: %w", err)
	}
	recorded, err := loadOperationByID(ctx, tx, operation.OperationID)
	if err != nil {
		return ExternalOperation{}, false, err
	}
	if err := tx.Commit(ctx); err != nil {
		return ExternalOperation{}, false, fmt.Errorf("commit admission: %w", err)
	}
	return recorded, true, nil
}

// Enrollment loads one Host enrollment with its removal lifecycle.
func (s *Store) Enrollment(ctx context.Context, hostID string) (ExternalEnrollment, error) {
	enrollment, err := loadEnrollment(ctx, s.db, hostID)
	if err != nil {
		return ExternalEnrollment{}, err
	}
	removal, err := loadLatestRemoval(ctx, s.db, hostID)
	if err != nil {
		return ExternalEnrollment{}, err
	}
	if removal != nil && removal.Generation != enrollment.Generation {
		// The removal belongs to an earlier generation. It remains durable
		// history and must not disable the current enrollment.
		removal = nil
	}
	applyRemoval(&enrollment, removal)
	return enrollment, nil
}

func higherRevision(candidate, current string) bool {
	candidateValue, candidateErr := strconv.ParseUint(candidate, 10, 64)
	currentValue, currentErr := strconv.ParseUint(current, 10, 64)
	if candidateErr != nil || currentErr != nil {
		return false
	}
	return candidateValue > currentValue
}

func revisionGeneration(revision string) int64 {
	value, err := strconv.ParseInt(revision, 10, 64)
	if err != nil || value < 1 {
		return 1
	}
	return value
}

const enrollmentColumns = `host_id, workspace_id, enrollment_revision, host_signing_public_key,
	       host_noise_public_key, operation_id, state, disabled_at,
	       current_credential_operation_id, credential_generation`

type rowScanner interface {
	Scan(dest ...any) error
}

// querier is the read surface shared by the pool and a transaction.
type querier interface {
	QueryRow(ctx context.Context, sql string, args ...any) pgx.Row
}

func scanEnrollment(row rowScanner, enrollment *ExternalEnrollment) error {
	if err := row.Scan(
		&enrollment.HostID, &enrollment.WorkspaceID, &enrollment.EnrollmentRevision,
		&enrollment.HostSigningPublicKey, &enrollment.HostNoisePublicKey,
		&enrollment.OperationID, &enrollment.State, &enrollment.DisabledAt,
		&enrollment.CurrentCredentialOperationID, &enrollment.CredentialGeneration); err != nil {
		return err
	}
	enrollment.Generation = revisionGeneration(enrollment.EnrollmentRevision)
	return nil
}

func loadEnrollment(ctx context.Context, q querier, hostID string) (ExternalEnrollment, error) {
	var enrollment ExternalEnrollment
	err := scanEnrollment(q.QueryRow(ctx, `SELECT `+enrollmentColumns+`
		FROM external_enrollments WHERE host_id = $1`, hostID), &enrollment)
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
	err = scanEnrollment(tx.QueryRow(ctx, `UPDATE external_enrollments SET state = 'enrollment_ready'
		WHERE host_id = $1 AND state = 'enrollment_pending' AND disabled_at IS NULL
		RETURNING `+enrollmentColumns, hostID), &enrollment)
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
// grant for that Host in one transaction. It is the legacy bodyless entry:
// a repeated call returns the original disabled_at and an empty revocation set.
func (s *Store) DisableEnrollment(ctx context.Context, hostID string, at time.Time) (DisabledEnrollment, error) {
	return s.DisableEnrollmentWithTarget(ctx, hostID, "", nil, at)
}

// DisableEnrollmentWithTarget is the durable Host removal. It validates the
// expected binding under the Host lock before any mutation, freezes the target
// and the exact revoked grant generations in a removal row, and is idempotent
// per removal operation id. A retry after the database commit or a lost broker
// acknowledgement returns the stored target and revocations without revoking a
// second time.
func (s *Store) DisableEnrollmentWithTarget(
	ctx context.Context,
	hostID string,
	removalOperationID string,
	expected *RemovalTarget,
	at time.Time,
) (DisabledEnrollment, error) {
	if at.IsZero() {
		at = time.Now().UTC()
	}
	tx, err := s.db.Begin(ctx)
	if err != nil {
		return DisabledEnrollment{}, fmt.Errorf("begin disable enrollment: %w", err)
	}
	defer func() { _ = tx.Rollback(ctx) }()

	if err := lockHostScope(ctx, tx, hostID); err != nil {
		return DisabledEnrollment{}, err
	}
	var enrollmentOperationID, workspaceID, enrollmentRevision string
	var disabledAt *time.Time
	err = tx.QueryRow(ctx, `
		SELECT operation_id, workspace_id, enrollment_revision, disabled_at
		FROM external_enrollments WHERE host_id = $1 FOR UPDATE`, hostID).Scan(
		&enrollmentOperationID, &workspaceID, &enrollmentRevision, &disabledAt)
	if errors.Is(err, pgx.ErrNoRows) {
		return DisabledEnrollment{}, ErrNotFound
	}
	if err != nil {
		return DisabledEnrollment{}, fmt.Errorf("load enrollment for disable: %w", err)
	}
	effective := RemovalTarget{
		WorkspaceID:           workspaceID,
		EnrollmentOperationID: enrollmentOperationID,
		EnrollmentRevision:    enrollmentRevision,
	}
	if expected != nil && !expected.matched(effective) {
		return DisabledEnrollment{}, ErrTargetMismatch
	}

	// A retry with the same removal operation id returns the frozen row and
	// lets the caller re-issue exactly the stored denials.
	if removalOperationID != "" {
		stored, loadErr := loadRemoval(ctx, tx, removalOperationID)
		if loadErr != nil && !errors.Is(loadErr, ErrNotFound) {
			return DisabledEnrollment{}, loadErr
		}
		if loadErr == nil {
			if stored.HostID != hostID || (expected != nil && !expected.matched(RemovalTarget{
				WorkspaceID: stored.WorkspaceID, EnrollmentOperationID: stored.EnrollmentOperationID,
				EnrollmentRevision: stored.EnrollmentRevision,
			})) {
				return DisabledEnrollment{}, ErrTargetMismatch
			}
			if err := tx.Commit(ctx); err != nil {
				return DisabledEnrollment{}, fmt.Errorf("commit removal replay: %w", err)
			}
			return disabledFromRemoval(stored), nil
		}
	}

	revocations := map[string]uint64{}
	if disabledAt == nil {
		if err := tx.QueryRow(ctx, `
			UPDATE external_enrollments
			SET disabled_at = $2, state = 'enrollment_disabled'
			WHERE host_id = $1
			RETURNING disabled_at`, hostID, at).Scan(&disabledAt); err != nil {
			return DisabledEnrollment{}, fmt.Errorf("disable enrollment: %w", err)
		}
		rows, err := tx.Query(ctx, `
			UPDATE grants SET revoked_at = COALESCE(revoked_at, $2), updated_at = $2,
			       revocation_generation = revocation_generation + 1
			WHERE host_id = $1 AND revoked_at IS NULL
			RETURNING device_id, grant_generation`, hostID, at)
		if err != nil {
			return DisabledEnrollment{}, fmt.Errorf("revoke disabled host grants: %w", err)
		}
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
			WHERE operation_id = $1`, enrollmentOperationID, at); err != nil {
			return DisabledEnrollment{}, fmt.Errorf("record enrollment disable: %w", err)
		}
	}

	newID := removalOperationID
	if newID == "" {
		newID = NewOperationID()
	}
	revocationsJSON, err := json.Marshal(revocations)
	if err != nil {
		return DisabledEnrollment{}, fmt.Errorf("record removal revocations: %w", err)
	}
	var frozenID string
	err = tx.QueryRow(ctx, `
		INSERT INTO external_removals
			(removal_operation_id, host_id, workspace_id, enrollment_operation_id,
			 enrollment_revision, generation, disabled_at, revocations)
		VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
		ON CONFLICT (removal_operation_id) DO NOTHING
		RETURNING removal_operation_id`,
		newID, hostID, effective.WorkspaceID, effective.EnrollmentOperationID,
		effective.EnrollmentRevision, revisionGeneration(effective.EnrollmentRevision),
		*disabledAt, revocationsJSON).Scan(&frozenID)
	if errors.Is(err, pgx.ErrNoRows) {
		stored, loadErr := loadRemoval(ctx, tx, newID)
		if loadErr != nil {
			return DisabledEnrollment{}, loadErr
		}
		if stored.HostID != hostID {
			return DisabledEnrollment{}, ErrTargetMismatch
		}
		if err := tx.Commit(ctx); err != nil {
			return DisabledEnrollment{}, fmt.Errorf("commit removal replay: %w", err)
		}
		return disabledFromRemoval(stored), nil
	}
	if err != nil {
		return DisabledEnrollment{}, fmt.Errorf("record removal: %w", err)
	}
	if err := tx.Commit(ctx); err != nil {
		return DisabledEnrollment{}, fmt.Errorf("commit disable enrollment: %w", err)
	}
	return DisabledEnrollment{
		HostID:             hostID,
		RemovalOperationID: newID,
		Target:             effective,
		Generation:         uint64(revisionGeneration(effective.EnrollmentRevision)),
		DisabledAt:         *disabledAt,
		Revocations:        revocations,
	}, nil
}

// MarkRemovalEnforcedAt records a matching broker acknowledgement and the
// exact connection instance it applied to. It only sets the columns once and
// never changes the frozen target or revoked generations.
func (s *Store) MarkRemovalEnforcedAt(ctx context.Context, removalOperationID string, applied *ConnectionRef, at time.Time) error {
	if removalOperationID == "" {
		return errors.New("store: removal operation id is required")
	}
	if at.IsZero() {
		at = time.Now().UTC()
	}
	var epoch any
	var id any
	if applied != nil && applied.Valid() {
		epoch = applied.Epoch
		id = int64(applied.ID)
	}
	tag, err := s.db.Exec(ctx, `
		UPDATE external_removals
		SET enforced_at = COALESCE(enforced_at, $2),
		    enforced_connection_epoch = COALESCE(enforced_connection_epoch, $3),
		    enforced_connection_id = COALESCE(enforced_connection_id, $4)
		WHERE removal_operation_id = $1`, removalOperationID, at, epoch, id)
	if err != nil {
		return fmt.Errorf("mark removal enforced: %w", err)
	}
	if tag.RowsAffected() == 0 {
		return ErrNotFound
	}
	return nil
}

// MarkRemovalEnforced records a matching broker acknowledgement. It only sets
// enforced_at once and never changes the frozen target or revoked generations.
func (s *Store) MarkRemovalEnforced(ctx context.Context, removalOperationID string, at time.Time) error {
	if removalOperationID == "" {
		return errors.New("store: removal operation id is required")
	}
	if at.IsZero() {
		at = time.Now().UTC()
	}
	tag, err := s.db.Exec(ctx, `
		UPDATE external_removals SET enforced_at = COALESCE(enforced_at, $2)
		WHERE removal_operation_id = $1`, removalOperationID, at)
	if err != nil {
		return fmt.Errorf("mark removal enforced: %w", err)
	}
	if tag.RowsAffected() == 0 {
		return ErrNotFound
	}
	return nil
}

// ReplaceHostCredential atomically records one credential-replacement
// operation and replaces the enrollment credential hash, the current issuance
// operation and the credential generation in one transaction.
func (s *Store) ReplaceHostCredential(ctx context.Context, operation ExternalOperation) (ExternalEnrollment, string, error) {
	if operation.OperationID == "" {
		return ExternalEnrollment{}, "", errors.New("store: operation id is required")
	}
	if err := s.ensureAdmission(ctx, operation); err != nil {
		return ExternalEnrollment{}, "", err
	}
	tx, err := s.db.Begin(ctx)
	if err != nil {
		return ExternalEnrollment{}, "", fmt.Errorf("begin credential replacement: %w", err)
	}
	defer func() { _ = tx.Rollback(ctx) }()

	if err := lockHostScope(ctx, tx, operation.HostID); err != nil {
		return ExternalEnrollment{}, "", err
	}
	admitted, err := loadOperationForUpdate(ctx, tx, operation.OperationID)
	if err != nil {
		return ExternalEnrollment{}, "", err
	}
	if admitted.Outcome == OperationCommitted {
		enrollment, loadErr := loadEnrollment(ctx, tx, operation.HostID)
		if loadErr != nil {
			return ExternalEnrollment{}, "", loadErr
		}
		if err := tx.Commit(ctx); err != nil {
			return ExternalEnrollment{}, "", fmt.Errorf("commit credential replay: %w", err)
		}
		return enrollment, "", nil
	}
	if admitted.Outcome == OperationRefused || admitted.Outcome == OperationExpired || admitted.Outcome == OperationFenced {
		return ExternalEnrollment{}, "", refusedOperationError(admitted)
	}
	if !bytes.Equal(admitted.PayloadDigest, operation.PayloadDigest) {
		return ExternalEnrollment{}, "", ErrOperationConflict
	}
	expired, err := operationAdmissionExpired(ctx, tx, admitted)
	if err != nil {
		return ExternalEnrollment{}, "", err
	}
	if expired {
		if err := markOperationOutcome(ctx, tx, admitted.OperationID, OperationExpired, "authorization_expired"); err != nil {
			return ExternalEnrollment{}, "", err
		}
		if err := tx.Commit(ctx); err != nil {
			return ExternalEnrollment{}, "", fmt.Errorf("commit expired operation: %w", err)
		}
		return ExternalEnrollment{}, "", ErrAuthorizationExpired
	}

	credential, err := newCredential()
	if err != nil {
		return ExternalEnrollment{}, "", err
	}
	var enrollment ExternalEnrollment
	err = scanEnrollment(tx.QueryRow(ctx, `
		UPDATE external_enrollments
		SET broker_credential_hash = $2, current_credential_operation_id = $3,
		    credential_generation = credential_generation + 1
		WHERE host_id = $1 AND workspace_id = $4 AND enrollment_revision = $5
		  AND disabled_at IS NULL AND state <> 'enrollment_disabled'
		RETURNING `+enrollmentColumns,
		operation.HostID, hashCredential(credential), admitted.OperationID,
		admitted.WorkspaceID, admitted.EnrollmentRevision), &enrollment)
	if errors.Is(err, pgx.ErrNoRows) {
		if _, loadErr := loadEnrollment(ctx, tx, operation.HostID); loadErr != nil {
			return ExternalEnrollment{}, "", loadErr
		}
		if err := markOperationOutcome(ctx, tx, admitted.OperationID, OperationRefused, "revision_mismatch"); err != nil {
			return ExternalEnrollment{}, "", err
		}
		if err := tx.Commit(ctx); err != nil {
			return ExternalEnrollment{}, "", fmt.Errorf("commit refusal: %w", err)
		}
		return ExternalEnrollment{}, "", ErrConflict
	}
	if err != nil {
		return ExternalEnrollment{}, "", fmt.Errorf("replace host credential: %w", err)
	}
	if _, err := tx.Exec(ctx, `
		UPDATE external_operations
		SET state = 'operation_committed', outcome = 'committed', reason = '',
		    committed_at = clock_timestamp(), updated_at = clock_timestamp(),
		    credential_issued = true, credential_generation = $2,
		    credential_operation_id = $1
		WHERE operation_id = $1`, admitted.OperationID, enrollment.CredentialGeneration); err != nil {
		return ExternalEnrollment{}, "", fmt.Errorf("record credential replacement outcome: %w", err)
	}
	if err := tx.Commit(ctx); err != nil {
		return ExternalEnrollment{}, "", fmt.Errorf("commit credential replacement: %w", err)
	}
	return enrollment, credential, nil
}

// Operation loads one operation record scoped by operation id only.
func (s *Store) Operation(ctx context.Context, operationID string) (ExternalOperation, error) {
	return loadOperationByID(ctx, s.db, operationID)
}

const operationColumns = `operation_id, authorization_id, purpose, workspace_id, region_id,
	       host_id, payload_digest, state, enrollment_revision, committed_at,
	       authorization_expires_at, expected_workspace_id, expected_enrollment_revision,
	       admitted_at, outcome, reason, updated_at, credential_issued,
	       credential_generation, credential_operation_id`

func scanOperation(row rowScanner, operation *ExternalOperation) error {
	return row.Scan(
		&operation.OperationID, &operation.AuthorizationID, &operation.Purpose,
		&operation.WorkspaceID, &operation.RegionID, &operation.HostID,
		&operation.PayloadDigest, &operation.State, &operation.EnrollmentRevision,
		&operation.CommittedAt, &operation.AuthorizationExpiresAt,
		&operation.ExpectedWorkspaceID, &operation.ExpectedEnrollmentRevision,
		&operation.AdmittedAt, &operation.Outcome, &operation.Reason,
		&operation.UpdatedAt, &operation.CredentialIssued,
		&operation.CredentialGeneration, &operation.CredentialOperationID)
}

func loadOperationByID(ctx context.Context, q querier, operationID string) (ExternalOperation, error) {
	var operation ExternalOperation
	err := scanOperation(q.QueryRow(ctx, `SELECT `+operationColumns+`
		FROM external_operations WHERE operation_id = $1`, operationID), &operation)
	if errors.Is(err, pgx.ErrNoRows) {
		return ExternalOperation{}, ErrNotFound
	}
	if err != nil {
		return ExternalOperation{}, fmt.Errorf("load operation: %w", err)
	}
	return operation, nil
}

func loadOperationByAuthorization(ctx context.Context, q querier, authorizationID string) (ExternalOperation, error) {
	var operation ExternalOperation
	err := scanOperation(q.QueryRow(ctx, `SELECT `+operationColumns+`
		FROM external_operations WHERE authorization_id = $1`, authorizationID), &operation)
	if errors.Is(err, pgx.ErrNoRows) {
		return ExternalOperation{}, ErrNotFound
	}
	if err != nil {
		return ExternalOperation{}, fmt.Errorf("load operation: %w", err)
	}
	return operation, nil
}

func loadOperationForUpdate(ctx context.Context, tx pgx.Tx, operationID string) (ExternalOperation, error) {
	var operation ExternalOperation
	err := scanOperation(tx.QueryRow(ctx, `SELECT `+operationColumns+`
		FROM external_operations WHERE operation_id = $1 FOR UPDATE`, operationID), &operation)
	if errors.Is(err, pgx.ErrNoRows) {
		return ExternalOperation{}, ErrNotFound
	}
	if err != nil {
		return ExternalOperation{}, fmt.Errorf("load operation for mutation: %w", err)
	}
	return operation, nil
}

func lockHostScope(ctx context.Context, tx pgx.Tx, hostID string) error {
	if _, err := tx.Exec(ctx, `SELECT pg_advisory_xact_lock(hashtextextended('whalerein-host:' || $1, 0))`, hostID); err != nil {
		return fmt.Errorf("lock host scope: %w", err)
	}
	return nil
}

// operationAdmissionExpired reports an operation that was admitted at or after
// its authorization deadline. An operation admitted before the deadline may
// still commit after it (AC-17 .commit-after-expiry); a later admission is
// fenced instead.
func operationAdmissionExpired(ctx context.Context, tx pgx.Tx, operation ExternalOperation) (bool, error) {
	if operation.AuthorizationExpiresAt == nil {
		return false, nil
	}
	var expired bool
	if err := tx.QueryRow(ctx, `
		SELECT clock_timestamp() >= $1
		   AND COALESCE($2::timestamptz, clock_timestamp()) >= $1`,
		operation.AuthorizationExpiresAt, operation.AdmittedAt).Scan(&expired); err != nil {
		return false, fmt.Errorf("check operation admission deadline: %w", err)
	}
	return expired, nil
}

func markOperationOutcome(ctx context.Context, tx pgx.Tx, operationID, outcome, reason string) error {
	state := "operation_admitted"
	switch outcome {
	case OperationCommitted:
		state = "operation_committed"
	case OperationRefused:
		state = "operation_refused"
	case OperationExpired:
		state = "operation_expired"
	case OperationFenced:
		state = "operation_fenced"
	}
	if _, err := tx.Exec(ctx, `
		UPDATE external_operations
		SET state = $2, outcome = $3, reason = $4, updated_at = clock_timestamp()
		WHERE operation_id = $1`, operationID, state, outcome, reason); err != nil {
		return fmt.Errorf("record operation outcome: %w", err)
	}
	return nil
}

func refusedOperationError(operation ExternalOperation) error {
	switch operation.Outcome {
	case OperationExpired:
		return ErrAuthorizationExpired
	case OperationFenced:
		return ErrOperationFenced
	case OperationRefused:
		switch operation.Reason {
		case "duplicate_host":
			return ErrDuplicateHost
		case "enrollment_disabled":
			return ErrEnrollmentDisabled
		}
		return ErrOperationRefused
	default:
		return ErrOperationPending
	}
}

func admissionMatches(recorded, incoming ExternalOperation) bool {
	return recorded.OperationID == incoming.OperationID &&
		recorded.AuthorizationID == incoming.AuthorizationID &&
		recorded.Purpose == incoming.Purpose &&
		recorded.WorkspaceID == incoming.WorkspaceID &&
		recorded.HostID == incoming.HostID &&
		recorded.EnrollmentRevision == incoming.EnrollmentRevision &&
		recorded.ExpectedWorkspaceID == incoming.ExpectedWorkspaceID &&
		recorded.ExpectedEnrollmentRevision == incoming.ExpectedEnrollmentRevision &&
		bytes.Equal(recorded.PayloadDigest, incoming.PayloadDigest)
}

func nullableTime(value *time.Time) any {
	if value == nil {
		return nil
	}
	return value.UTC()
}

func loadRemoval(ctx context.Context, q querier, removalOperationID string) (removalRow, error) {
	var removal removalRow
	var revocations []byte
	err := q.QueryRow(ctx, `
		SELECT removal_operation_id, host_id, workspace_id, enrollment_operation_id,
		       enrollment_revision, disabled_at, enforced_at, revocations
		FROM external_removals WHERE removal_operation_id = $1`, removalOperationID).Scan(
		&removal.RemovalOperationID, &removal.HostID, &removal.WorkspaceID,
		&removal.EnrollmentOperationID, &removal.EnrollmentRevision,
		&removal.DisabledAt, &removal.EnforcedAt, &revocations)
	if errors.Is(err, pgx.ErrNoRows) {
		return removalRow{}, ErrNotFound
	}
	if err != nil {
		return removalRow{}, fmt.Errorf("load removal: %w", err)
	}
	removal.Generation = revisionGeneration(removal.EnrollmentRevision)
	removal.Revocations = map[string]uint64{}
	if len(revocations) > 0 {
		if err := json.Unmarshal(revocations, &removal.Revocations); err != nil {
			return removalRow{}, fmt.Errorf("decode removal revocations: %w", err)
		}
	}
	return removal, nil
}

func loadLatestRemoval(ctx context.Context, q querier, hostID string) (*removalRow, error) {
	var removal removalRow
	var revocations []byte
	err := q.QueryRow(ctx, `
		SELECT removal_operation_id, host_id, workspace_id, enrollment_operation_id,
		       enrollment_revision, disabled_at, enforced_at, revocations
		FROM external_removals WHERE host_id = $1
		ORDER BY disabled_at DESC, created_at DESC LIMIT 1`, hostID).Scan(
		&removal.RemovalOperationID, &removal.HostID, &removal.WorkspaceID,
		&removal.EnrollmentOperationID, &removal.EnrollmentRevision,
		&removal.DisabledAt, &removal.EnforcedAt, &revocations)
	if errors.Is(err, pgx.ErrNoRows) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("load latest removal: %w", err)
	}
	removal.Generation = revisionGeneration(removal.EnrollmentRevision)
	removal.Revocations = map[string]uint64{}
	if len(revocations) > 0 {
		if err := json.Unmarshal(revocations, &removal.Revocations); err != nil {
			return nil, fmt.Errorf("decode removal revocations: %w", err)
		}
	}
	return &removal, nil
}

func applyRemoval(enrollment *ExternalEnrollment, removal *removalRow) {
	switch {
	case removal != nil && removal.EnforcedAt != nil:
		enrollment.RemovalState = "enforced"
	case removal != nil:
		enrollment.RemovalState = "pending"
	case enrollment.DisabledAt != nil || enrollment.State == "enrollment_disabled":
		enrollment.RemovalState = "disabled"
	default:
		enrollment.RemovalState = "none"
	}
	if removal != nil {
		enrollment.RemovalOperationID = removal.RemovalOperationID
		enrollment.RemovalEnforcedAt = removal.EnforcedAt
	}
}

func disabledFromRemoval(removal removalRow) DisabledEnrollment {
	revocations := removal.Revocations
	if revocations == nil {
		revocations = map[string]uint64{}
	}
	generation := uint64(0)
	if removal.Generation > 0 {
		generation = uint64(removal.Generation)
	}
	return DisabledEnrollment{
		HostID:             removal.HostID,
		RemovalOperationID: removal.RemovalOperationID,
		Target: RemovalTarget{
			WorkspaceID:           removal.WorkspaceID,
			EnrollmentOperationID: removal.EnrollmentOperationID,
			EnrollmentRevision:    removal.EnrollmentRevision,
		},
		Generation:  generation,
		DisabledAt:  removal.DisabledAt,
		EnforcedAt:  removal.EnforcedAt,
		Revocations: revocations,
	}
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
