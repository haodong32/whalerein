package store

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"
)

var requestColumnsR = prefixColumns(requestColumns, "r.")

func prefixColumns(columns, prefix string) string {
	parts := strings.Split(columns, ",")
	for i := range parts {
		parts[i] = prefix + strings.TrimSpace(parts[i])
	}
	return strings.Join(parts, ", ")
}

// CreateRequest inserts a request row or returns the existing row for the same
// (host, device, command_id). A different digest is reported without writing.
func (s *Store) CreateRequest(ctx context.Context, req Request) (CreateOutcome, error) {
	if req.ClientTransportVersion != 0 && req.ClientTransportVersion != 2 {
		return CreateOutcome{}, errors.New("invalid receiver transport capability")
	}
	if !validStatus(req.Status) {
		return CreateOutcome{}, fmt.Errorf("create request: invalid status %q", req.Status)
	}
	if req.ForwardState == "" {
		req.ForwardState = ForwardPending
	}
	switch req.ForwardState {
	case ForwardNone, ForwardPending, ForwardPublished:
	default:
		return CreateOutcome{}, fmt.Errorf("create request: invalid forward state %q", req.ForwardState)
	}
	if req.CreatedAt.IsZero() {
		req.CreatedAt = time.Now().UTC()
	}
	if req.UpdatedAt.IsZero() {
		req.UpdatedAt = req.CreatedAt
	}
	if req.ForwardAfter.IsZero() {
		req.ForwardAfter = req.CreatedAt
	}
	if req.PayloadDigest == nil {
		req.PayloadDigest = []byte{}
	}
	if req.RequestBody == nil {
		req.RequestBody = []byte{}
	}
	if req.ResultBody == nil {
		req.ResultBody = []byte{}
	}

	args := []any{
		req.RequestID,
		req.HostID,
		req.DeviceID,
		req.CommandID,
		req.PayloadDigest,
		int32(req.Kind),
		int32(req.ProtocolVersion),
		StatusName(req.Status),
		req.RequestBody,
		req.ResultBody,
		int32(req.ErrorCode),
		req.ErrorMessage,
		req.ChannelID,
		req.TraceID,
		req.ExpiresAt,
		string(req.ForwardState),
		req.ForwardAfter,
		req.ForwardAttempts,
		req.PublishedOnce,
		req.CreatedAt,
		req.UpdatedAt,
		int32(req.ClientTransportVersion),
	}

	if req.CommandID != "" {
		insertSQL := `
			INSERT INTO requests (` + requestColumns + `)
			VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22)
			ON CONFLICT (host_id, device_id, command_id) WHERE command_id <> '' DO NOTHING
			RETURNING ` + requestColumns
		row := s.pool.QueryRow(ctx, insertSQL, args...)
		inserted, err := scanRequest(row)
		switch {
		case err == nil:
			return CreateOutcome{Request: inserted, Created: true}, nil
		case errors.Is(err, pgx.ErrNoRows):
			existing, err := s.requestByCommand(ctx, req.HostID, req.DeviceID, req.CommandID)
			if err != nil {
				return CreateOutcome{}, err
			}
			if !bytes.Equal(existing.PayloadDigest, req.PayloadDigest) {
				return CreateOutcome{Request: existing, DigestMismatch: true}, nil
			}
			return CreateOutcome{Request: existing}, nil
		case isUniqueViolation(err):
			return CreateOutcome{}, fmt.Errorf("create request: %w", ErrConflict)
		default:
			return CreateOutcome{}, fmt.Errorf("create request: %w", err)
		}
	}

	insertSQL := `
		INSERT INTO requests (` + requestColumns + `)
		VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22)
		RETURNING ` + requestColumns
	row := s.pool.QueryRow(ctx, insertSQL, args...)
	inserted, err := scanRequest(row)
	if err != nil {
		if isUniqueViolation(err) {
			return CreateOutcome{}, fmt.Errorf("create request: %w", ErrConflict)
		}
		return CreateOutcome{}, fmt.Errorf("create request: %w", err)
	}
	return CreateOutcome{Request: inserted, Created: true}, nil
}

func (s *Store) requestByCommand(ctx context.Context, hostID, deviceID, commandID string) (Request, error) {
	row := s.pool.QueryRow(ctx, `
		SELECT `+requestColumns+` FROM requests
		WHERE host_id = $1 AND device_id = $2 AND command_id = $3`, hostID, deviceID, commandID)
	req, err := scanRequest(row)
	if err != nil {
		return Request{}, wrapRequestScan(err)
	}
	return req, nil
}

// GetRequest reads one request state copy by primary key.
func (s *Store) GetRequest(ctx context.Context, requestID string) (Request, error) {
	row := s.pool.QueryRow(ctx, `SELECT `+requestColumns+` FROM requests WHERE request_id = $1`, requestID)
	req, err := scanRequest(row)
	if err != nil {
		return Request{}, wrapRequestScan(err)
	}
	return req, nil
}

// ClaimDueRequests leases due pending rows with FOR UPDATE SKIP LOCKED. The
// transaction commits before the caller publishes; the lease is pushed out so
// a crashed publisher is retried after expiry.
func (s *Store) ClaimDueRequests(ctx context.Context, now time.Time, limit int, lease time.Duration) ([]Request, error) {
	if limit <= 0 {
		return nil, nil
	}
	query := `
		WITH due AS (
			SELECT request_id FROM requests
			WHERE status = 'received' AND forward_state = 'pending' AND forward_after <= $1 AND expires_at > $1
			ORDER BY forward_after
			LIMIT $2
			FOR UPDATE SKIP LOCKED
		)
		UPDATE requests r
		SET forward_after = $3, forward_attempts = r.forward_attempts + 1, updated_at = $1
		FROM due
		WHERE r.request_id = due.request_id
		RETURNING ` + requestColumnsR
	rows, err := s.pool.Query(ctx, query, now, limit, now.Add(lease))
	if err != nil {
		return nil, fmt.Errorf("claim due requests: %w", err)
	}
	defer rows.Close()
	out := make([]Request, 0, limit)
	for rows.Next() {
		req, err := scanRequest(rows)
		if err != nil {
			return nil, err
		}
		out = append(out, req)
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("claim due requests: %w", err)
	}
	return out, nil
}

// Internal pairing COMMIT rows (kind 5) retry until their deadline, rather
// than exhausting a short command-delivery attempt budget.
// RequeueDuePublished makes still-received published rows due again so an
// unacknowledged command can be retried. Terminal rows are never requeued.
func (s *Store) RequeueDuePublished(ctx context.Context, now time.Time, maxAttempts int) (int64, error) {
	cmd, err := s.pool.Exec(ctx, `
		UPDATE requests
		SET forward_state = 'pending', forward_after = $1, updated_at = $1
		WHERE status = 'received'
		  AND forward_state = 'published'
		  AND published_once
		  AND forward_after <= $2
		  AND (forward_attempts < $3 OR kind = 5) AND expires_at > $1`, now, now, maxAttempts)
	if err != nil {
		return 0, fmt.Errorf("requeue published requests: %w", err)
	}
	return cmd.RowsAffected(), nil
}

// MarkPublished records a successful MQTT publish. A later retry re-publishes
// the same row until the host writes a result or attempts are exhausted.
func (s *Store) MarkPublished(ctx context.Context, requestID string, now time.Time, retryAfter time.Duration) error {
	_, err := s.pool.Exec(ctx, `
		UPDATE requests
		SET forward_state = 'published', published_once = true,
		    forward_after = $2, updated_at = $3
		WHERE request_id = $1 AND status = 'received'`,
		requestID, now.Add(retryAfter), now)
	if err != nil {
		return fmt.Errorf("mark request published: %w", err)
	}
	return nil
}

// MarkPublishFailure either schedules another attempt or, for a request that
// was never published, moves it to host_offline after the attempt budget. A
// row that was published at least once stays received until expiry.
func (s *Store) MarkPublishFailure(ctx context.Context, requestID string, now time.Time, retryAfter time.Duration, maxAttempts int) error {
	_, err := s.pool.Exec(ctx, `
		UPDATE requests
		SET status = CASE
                WHEN kind = 5 THEN status
		        WHEN forward_attempts >= $2 AND NOT published_once THEN 'host_offline'
		        ELSE status END,
		    forward_state = CASE
                WHEN kind = 5 THEN 'pending'
		        WHEN forward_attempts >= $2 AND published_once THEN 'published'
		        WHEN forward_attempts >= $2 THEN 'none'
		        ELSE 'pending' END,
		    forward_after = CASE
                WHEN kind = 5 THEN $3
		        WHEN forward_attempts >= $2 THEN forward_after
		        ELSE $3 END,
		    updated_at = $4
		WHERE request_id = $1 AND status = 'received'`,
		requestID, maxAttempts, now.Add(retryAfter), now)
	if err != nil {
		return fmt.Errorf("mark request publish failure: %w", err)
	}
	return nil
}

// ApplyHostResult conditionally updates the same request row. The WHERE clause
// encodes exactly the allowed online status transitions.
func (s *Store) ApplyHostResult(ctx context.Context, update HostResultUpdate) (bool, error) {
	if !validStatus(update.Status) {
		return false, fmt.Errorf("apply host result: invalid status %q", update.Status)
	}
	if update.ResultBody == nil {
		update.ResultBody = []byte{}
	}
	cmd, err := s.pool.Exec(ctx, `
		UPDATE requests
		SET status = $4,
		    result_body = CASE WHEN expires_at > $9 THEN $5 ELSE result_body END,
		    error_code = $6,
		    error_message = $7,
		    channel_id = CASE WHEN $8 <> '' THEN $8 ELSE channel_id END,
		    forward_state = 'none',
		    updated_at = $9
		WHERE request_id = $1 AND host_id = $2 AND device_id = $3
		  AND (
		      (status = 'received' AND $4 IN ('host_offline','rejected','host_accepted','succeeded','failed','unknown'))
		      OR (status = 'host_accepted' AND $4 IN ('succeeded','failed','unknown'))
		      OR (status = 'unknown' AND $4 IN ('succeeded','failed'))
		  )`,
		update.RequestID,
		update.HostID,
		update.DeviceID,
		StatusName(update.Status),
		update.ResultBody,
		int32(update.ErrorCode),
		update.ErrorMessage,
		update.ChannelID,
		update.At,
	)
	if err != nil {
		return false, fmt.Errorf("apply host result: %w", err)
	}
	return cmd.RowsAffected() == 1, nil
}

// ClearExpiredBodies drops request and result bodies after expiry while
// preserving the routing identity and terminal status copy.
func (s *Store) ClearExpiredBodies(ctx context.Context, now time.Time) (int64, error) {
	cmd, err := s.pool.Exec(ctx, `
		UPDATE requests
		SET request_body = ''::bytea, result_body = ''::bytea, updated_at = $1
		WHERE expires_at <= $1
		  AND (request_body <> ''::bytea OR result_body <> ''::bytea)`, now)
	if err != nil {
		return 0, fmt.Errorf("clear expired request bodies: %w", err)
	}
	return cmd.RowsAffected(), nil
}

// CountRequests is a component-test helper.
func (s *Store) CountRequests(ctx context.Context) (int, error) {
	var count int
	if err := s.pool.QueryRow(ctx, `SELECT count(*) FROM requests`).Scan(&count); err != nil {
		return 0, fmt.Errorf("count requests: %w", err)
	}
	return count, nil
}

// DeleteExpiredQueries removes a bounded batch of old, non-control records.
// Command and activation identities remain available for dedupe and recovery.
func (s *Store) DeleteExpiredQueries(ctx context.Context, before time.Time) (int64, error) {
	result, err := s.pool.Exec(ctx, `DELETE FROM requests WHERE request_id IN (
        SELECT request_id FROM requests WHERE expires_at <= $1 AND command_id=''
        AND kind IN (1,2) ORDER BY expires_at,request_id LIMIT 2000)`, before)
	if err != nil {
		return 0, fmt.Errorf("delete expired query records: %w", err)
	}
	return result.RowsAffected(), nil
}
