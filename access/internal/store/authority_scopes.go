package store

import (
	"context"
	"fmt"
	"time"
)

// AuthorityScope is one live Device session that has a grant and an immutable
// App-session proof key. Access requests a Central permit for exactly this
// tuple; no identity or route is inferred from a token.
type AuthorityScope struct {
	HostID          string
	DeviceID        string
	SessionID       string
	WorkspaceID     string
	GrantGeneration uint64
	ProofPublicKey  []byte
	ExpiresAt       time.Time
}

// AuthorityScopes lists live managed sessions eligible for a Central permit.
func (s *Store) AuthorityScopes(ctx context.Context, now time.Time) ([]AuthorityScope, error) {
	if s == nil || s.db == nil {
		return nil, ErrUnavailable
	}
	rows, err := s.db.Query(ctx, `
		SELECT g.host_id, ds.device_id, ds.session_id, ds.workspace_id,
		       g.grant_generation, ds.proof_public_key, ds.expires_at
		FROM device_sessions ds JOIN grants g ON g.device_id=ds.device_id
		WHERE NOT ds.revoked AND ds.expires_at>$1 AND g.revoked_at IS NULL
		  AND g.grant_generation>0 AND length(ds.proof_public_key)=32
		ORDER BY g.host_id, ds.device_id, ds.session_id`, now)
	if err != nil {
		return nil, fmt.Errorf("list authority scopes: %w", err)
	}
	defer rows.Close()
	scopes := make([]AuthorityScope, 0)
	for rows.Next() {
		var scope AuthorityScope
		var generation int64
		if err := rows.Scan(&scope.HostID, &scope.DeviceID, &scope.SessionID, &scope.WorkspaceID,
			&generation, &scope.ProofPublicKey, &scope.ExpiresAt); err != nil {
			return nil, fmt.Errorf("scan authority scope: %w", err)
		}
		scope.GrantGeneration = uint64(generation)
		scopes = append(scopes, scope)
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("list authority scopes: %w", err)
	}
	return scopes, nil
}
