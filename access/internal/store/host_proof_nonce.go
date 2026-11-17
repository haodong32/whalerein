package store

import (
	"context"
	"errors"
	"fmt"
	"time"
)

// HostProofNonceRetention bounds how long one proof nonce is remembered. A
// proof is only accepted within a five-minute issue window, so a longer
// retention guarantees a captured proof cannot become usable again after its
// row is pruned.
const HostProofNonceRetention = 15 * time.Minute

// ConsumeHostProofNonce records one Host ownership proof nonce and reports
// whether this call was the first to use it. The composite primary key makes
// the guard durable across Access replicas, and the purpose separates the
// status and credential-replacement proof namespaces.
func (s *Store) ConsumeHostProofNonce(ctx context.Context, nonceHash []byte, purpose string, at time.Time) (bool, error) {
	if s == nil || s.db == nil {
		return false, errors.New("store: no database pool")
	}
	if len(nonceHash) != 32 {
		return false, errors.New("consume host proof nonce: nonce hash must be 32 bytes")
	}
	if purpose == "" {
		return false, errors.New("consume host proof nonce: purpose is required")
	}
	if at.IsZero() {
		at = time.Now().UTC()
	}
	// Opportunistic retention keeps the table bounded without a worker.
	if _, err := s.db.Exec(ctx, `DELETE FROM host_proof_nonces WHERE created_at < $1`,
		at.Add(-HostProofNonceRetention)); err != nil {
		return false, fmt.Errorf("prune host proof nonces: %w", err)
	}
	command, err := s.db.Exec(ctx, `
		INSERT INTO host_proof_nonces (purpose, nonce_hash, created_at)
		VALUES ($1, $2, $3)
		ON CONFLICT (purpose, nonce_hash) DO NOTHING`,
		purpose, nonceHash, at)
	if err != nil {
		return false, fmt.Errorf("consume host proof nonce: %w", err)
	}
	return command.RowsAffected() == 1, nil
}
