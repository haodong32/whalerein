package store

import (
	"context"
	"time"
)

// PairingOperation contains only public handshake data and credential hashes.
// FinishResponse never contains a plaintext transport credential.
type PairingOperation struct {
	ClaimID         string
	TokenHash       []byte
	HostID          string
	DeviceID        string
	HelloHash       []byte
	HelloResponse   []byte
	ExpiresAt       time.Time
	FinishHash      []byte
	FinishResponse  []byte
	CommitRequestID string
	GrantGeneration uint64
	AuthHash        []byte
	Active          bool
}

// SavePairingOperation reserves a claim ID and its immutable handshake binding.
func (s *Store) SavePairingOperation(ctx context.Context, op PairingOperation) error {
	_, err := s.pool.Exec(ctx, `INSERT INTO pairing_operations
        (claim_id,token_hash,host_id,device_id,hello_hash,hello_response,expires_at)
        VALUES($1,$2,$3,$4,$5,$6,$7)`, op.ClaimID, op.TokenHash, op.HostID, op.DeviceID, op.HelloHash, op.HelloResponse, op.ExpiresAt)
	if isUniqueViolation(err) {
		return ErrConflict
	}
	return err
}

// PairingOperation reads progress and verifies the exact committed grant is live.
func (s *Store) PairingOperation(ctx context.Context, claimID string) (PairingOperation, error) {
	var op PairingOperation
	err := s.pool.QueryRow(ctx, `SELECT p.claim_id,p.token_hash,p.host_id,p.device_id,p.hello_hash,p.hello_response,p.expires_at,
        p.finish_hash,p.finish_response,COALESCE(p.commit_request_id,''),COALESCE(p.grant_generation,0),p.auth_hash,
        EXISTS(SELECT 1 FROM grants g JOIN device_credentials c ON c.device_id=g.device_id
            WHERE g.host_id=p.host_id AND g.device_id=p.device_id AND g.revoked_at IS NULL
            AND g.grant_generation=p.grant_generation AND c.auth_hash=p.auth_hash)
        FROM pairing_operations p WHERE claim_id=$1`, claimID).Scan(&op.ClaimID, &op.TokenHash, &op.HostID, &op.DeviceID,
		&op.HelloHash, &op.HelloResponse, &op.ExpiresAt, &op.FinishHash, &op.FinishResponse, &op.CommitRequestID,
		&op.GrantGeneration, &op.AuthHash, &op.Active)
	return op, wrapRequestScan(err)
}
