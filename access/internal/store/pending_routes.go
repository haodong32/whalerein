package store

import (
	"context"
	"fmt"
	"time"
)

// PendingRoute is one route control operation that Access has not yet had
// accepted by Central.
type PendingRoute struct {
	HostID     string
	Kind       string
	Generation uint64
	Payload    []byte
	UpdatedAt  time.Time
}

const (
	PendingAttach = "attach"
	PendingClose  = "close"
)

// SavePendingRoute upserts one terminal/attach operation by Host.
func (s *Store) SavePendingRoute(ctx context.Context, route PendingRoute) error {
	if s == nil || s.db == nil {
		return ErrUnavailable
	}
	if route.HostID == "" || route.Generation == 0 ||
		(route.Kind != PendingAttach && route.Kind != PendingClose) || len(route.Payload) == 0 {
		return fmt.Errorf("save pending route: invalid scope")
	}
	_, err := s.db.Exec(ctx, `
		INSERT INTO pending_routes (host_id, kind, generation, payload, updated_at)
		VALUES ($1,$2,$3,$4,clock_timestamp())
		ON CONFLICT (host_id) DO UPDATE SET
		    kind=excluded.kind, generation=excluded.generation,
		    payload=excluded.payload, updated_at=excluded.updated_at`,
		route.HostID, route.Kind, int64(route.Generation), route.Payload)
	if err != nil {
		return fmt.Errorf("save pending route: %w", err)
	}
	return nil
}

// DeletePendingRoute removes an accepted operation only when the stored
// generation is not newer than the one that succeeded.
func (s *Store) DeletePendingRoute(ctx context.Context, hostID string, generation uint64) error {
	if s == nil || s.db == nil {
		return ErrUnavailable
	}
	_, err := s.db.Exec(ctx, `DELETE FROM pending_routes WHERE host_id=$1 AND generation <= $2`, hostID, int64(generation))
	if err != nil {
		return fmt.Errorf("delete pending route: %w", err)
	}
	return nil
}

// ListPendingRoutes reads the durable retry queue in Host order.
func (s *Store) ListPendingRoutes(ctx context.Context) ([]PendingRoute, error) {
	if s == nil || s.db == nil {
		return nil, ErrUnavailable
	}
	rows, err := s.db.Query(ctx, `SELECT host_id,kind,generation,payload,updated_at
		FROM pending_routes ORDER BY host_id`)
	if err != nil {
		return nil, fmt.Errorf("list pending routes: %w", err)
	}
	defer rows.Close()
	routes := make([]PendingRoute, 0)
	for rows.Next() {
		var route PendingRoute
		var generation int64
		if err := rows.Scan(&route.HostID, &route.Kind, &generation, &route.Payload, &route.UpdatedAt); err != nil {
			return nil, fmt.Errorf("scan pending route: %w", err)
		}
		route.Generation = uint64(generation)
		routes = append(routes, route)
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("list pending routes: %w", err)
	}
	return routes, nil
}
