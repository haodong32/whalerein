package store

import (
	"context"
	"fmt"
	"time"
)

// CreatePairingToken stores only the SHA-256 hash of a one-time invitation.
func (s *Store) CreatePairingToken(ctx context.Context, tokenHash []byte, hostID string, expiresAt time.Time, at time.Time) error {
	_, err := s.db.Exec(ctx, `
		INSERT INTO pairing_tokens (token_hash, host_id, expires_at, created_at, updated_at)
		VALUES ($1, $2, $3, $4, $4)`,
		tokenHash, hostID, expiresAt, at)
	if err != nil {
		if isUniqueViolation(err) {
			return fmt.Errorf("create pairing token: %w", ErrConflict)
		}
		return fmt.Errorf("create pairing token: %w", err)
	}
	return nil
}

// PairingTokenByHash reads a token row by its hash.
func (s *Store) PairingTokenByHash(ctx context.Context, tokenHash []byte) (PairingToken, error) {
	var token PairingToken
	err := s.db.QueryRow(ctx, `
		SELECT token_hash, host_id, expires_at, used_at, failure_count,
		       last_failure_at, created_at, updated_at
		FROM pairing_tokens WHERE token_hash = $1`, tokenHash).Scan(
		&token.TokenHash,
		&token.HostID,
		&token.ExpiresAt,
		&token.UsedAt,
		&token.FailureCount,
		&token.LastFailureAt,
		&token.CreatedAt,
		&token.UpdatedAt,
	)
	if err != nil {
		return PairingToken{}, wrapRequestScan(err)
	}
	return token, nil
}

// IncrementPairingFailure consumes one failure from the token budget.
func (s *Store) IncrementPairingFailure(ctx context.Context, tokenHash []byte, at time.Time) (int, error) {
	var count int
	err := s.db.QueryRow(ctx, `
		UPDATE pairing_tokens
		SET failure_count = failure_count + 1, last_failure_at = $2, updated_at = $2
		WHERE token_hash = $1
		RETURNING failure_count`, tokenHash, at).Scan(&count)
	if err != nil {
		return 0, wrapRequestScan(err)
	}
	return count, nil
}

// MarkPairingTokenUsed sets used_at exactly once. It returns false when the
// token was already used; used_at is retained.
func (s *Store) MarkPairingTokenUsed(ctx context.Context, tokenHash []byte, at time.Time) (bool, error) {
	cmd, err := s.db.Exec(ctx, `
		UPDATE pairing_tokens SET used_at = $2, updated_at = $2
		WHERE token_hash = $1 AND used_at IS NULL`, tokenHash, at)
	if err != nil {
		return false, fmt.Errorf("mark pairing token used: %w", err)
	}
	return cmd.RowsAffected() == 1, nil
}

// DeleteExpiredPairingTokens removes invitations that expired before the
// retention cutoff. Used rows are retained until the same cutoff so a second
// use of a recently used token is still rejected with its used_at information.
func (s *Store) DeleteExpiredPairingTokens(ctx context.Context, before time.Time) (int64, error) {
	if err := s.reconcilePairingOutcomes(ctx, before, true); err != nil {
		return 0, fmt.Errorf("reconcile pairing outcomes: %w", err)
	}
	if _, err := s.db.Exec(ctx, `DELETE FROM pairing_operations WHERE expires_at <= $1`, before); err != nil {
		return 0, err
	}
	cmd, err := s.db.Exec(ctx, `DELETE FROM pairing_tokens WHERE expires_at <= $1`, before)
	if err != nil {
		return 0, fmt.Errorf("delete expired pairing tokens: %w", err)
	}
	return cmd.RowsAffected(), nil
}
