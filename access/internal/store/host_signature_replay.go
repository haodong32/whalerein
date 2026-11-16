package store

import (
	"context"
	"errors"
	"fmt"
	"time"
)

// HostSignatureReplayRetention bounds how long one consumed signature is kept.
// It covers the entire acceptance window plus scheduling slack; a signature
// outside that window is already rejected by the clock check.
const HostSignatureReplayRetention = 5 * time.Minute

// ConsumeHostSignature records one verified Host authorization signature and
// reports whether this call was the first to use it. The database primary key
// is the durable guard, so two Access replicas sharing this database cannot
// both accept the same signature.
func (s *Store) ConsumeHostSignature(ctx context.Context, signatureHash []byte, hostID, purpose, deviceID string, signedAtUnixMs int64, at time.Time) (bool, error) {
	if s == nil || s.db == nil {
		return false, errors.New("store: no database pool")
	}
	if len(signatureHash) != 32 {
		return false, errors.New("consume host signature: signature hash must be 32 bytes")
	}
	if hostID == "" || purpose == "" {
		return false, errors.New("consume host signature: host id and purpose are required")
	}
	if at.IsZero() {
		at = time.Now().UTC()
	}
	// Opportunistic retention keeps the table bounded without a separate
	// worker. Host authorization is rare, so the extra DELETE is cheap.
	if _, err := s.db.Exec(ctx, `DELETE FROM host_signature_replays WHERE created_at < $1`,
		at.Add(-HostSignatureReplayRetention)); err != nil {
		return false, fmt.Errorf("prune host signatures: %w", err)
	}
	command, err := s.db.Exec(ctx, `
		INSERT INTO host_signature_replays (signature_hash, host_id, purpose, device_id, signed_at_unix_ms, created_at)
		VALUES ($1, $2, $3, $4, $5, $6)
		ON CONFLICT (signature_hash) DO NOTHING`,
		signatureHash, hostID, purpose, deviceID, signedAtUnixMs, at)
	if err != nil {
		return false, fmt.Errorf("consume host signature: %w", err)
	}
	return command.RowsAffected() == 1, nil
}
