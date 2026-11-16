package store

import (
	"context"
	"fmt"
)

// WriteProbe proves that the database accepts a write transaction without
// changing any data. A read-only replica, a connection forced into
// default_transaction_read_only, or an exhausted pool fails here even though
// Ping still succeeds, so /readyz can keep the instance out of rotation.
func (s *Store) WriteProbe(ctx context.Context) error {
	tx, err := s.poolBegin(ctx)
	if err != nil {
		return fmt.Errorf("write probe: %w", err)
	}
	defer func() { _ = tx.Rollback(ctx) }()
	// WHERE false takes the table write intent without touching a row.
	if _, err := tx.Exec(ctx, `UPDATE schema_migrations SET applied_at = applied_at WHERE false`); err != nil {
		return fmt.Errorf("write probe: %w", err)
	}
	return nil
}
