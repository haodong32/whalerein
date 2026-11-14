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
	var enrollment ExternalEnrollment
	err := s.db.QueryRow(ctx, `
		SELECT host_id, workspace_id, enrollment_revision, host_signing_public_key,
		       host_noise_public_key, operation_id, state, disabled_at
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
