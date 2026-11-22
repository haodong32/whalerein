package store

import (
	"context"
	"errors"
	"fmt"
	"time"

	"github.com/jackc/pgx/v5"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// HeartbeatFailure is the typed reason an exact-request heartbeat did not
// renew. It is not an outcome of the request itself: a failed renewal never
// rewrites the recorded status.
type HeartbeatFailure string

// Heartbeat failure reasons.
const (
	HeartbeatNotFound      HeartbeatFailure = "not_found"
	HeartbeatTerminal      HeartbeatFailure = "terminal"
	HeartbeatHardDeadline  HeartbeatFailure = "hard_deadline"
	HeartbeatLeaseElapsed  HeartbeatFailure = "lease_elapsed"
	HeartbeatSessionEnded  HeartbeatFailure = "session_ended"
	HeartbeatDeviceForeign HeartbeatFailure = "foreign_device"
)

// HeartbeatOutcome reports one exact heartbeat decision.
type HeartbeatOutcome struct {
	Request Request
	Renewed bool
	Failure HeartbeatFailure
}

// HeartbeatRequest renews only the exact live, non-terminal, unexpired request
// for the authenticated device and session. It takes the row lock, classifies
// the current durable state and updates the lease in the same transaction.
// The lease is LEAST(expires_at, ...), so a heartbeat never extends the hard
// deadline. A terminal row, elapsed lease, ended session or elapsed hard
// deadline is reported explicitly and is never revived.
func (s *Store) HeartbeatRequest(ctx context.Context, deviceID, sessionID, requestID string, lease time.Duration) (HeartbeatOutcome, error) {
	if deviceID == "" || requestID == "" || lease <= 0 {
		return HeartbeatOutcome{}, errors.New("heartbeat: invalid scope")
	}
	tx, err := s.poolBegin(ctx)
	if err != nil {
		return HeartbeatOutcome{}, err
	}
	defer func() { _ = tx.Rollback(ctx) }()
	row := tx.QueryRow(ctx, `SELECT `+requestColumns+` FROM requests WHERE request_id = $1 FOR UPDATE`, requestID)
	req, err := scanRequest(row)
	if errors.Is(err, pgx.ErrNoRows) {
		return HeartbeatOutcome{Failure: HeartbeatNotFound}, nil
	}
	if err != nil {
		return HeartbeatOutcome{}, wrapRequestScan(err)
	}
	if req.DeviceID != deviceID {
		return HeartbeatOutcome{Failure: HeartbeatDeviceForeign}, nil
	}
	var databaseNow time.Time
	if err := tx.QueryRow(ctx, `SELECT clock_timestamp()`).Scan(&databaseNow); err != nil {
		return HeartbeatOutcome{}, err
	}
	if !databaseNow.Before(req.ExpiresAt) {
		return HeartbeatOutcome{Request: req, Failure: HeartbeatHardDeadline}, nil
	}
	switch req.Status {
	case whalereinv1.OnlineStatus_received, whalereinv1.OnlineStatus_host_accepted:
	default:
		return HeartbeatOutcome{Request: req, Failure: HeartbeatTerminal}, nil
	}
	if req.ClientLeaseState != ClientLeaseLive {
		return HeartbeatOutcome{Request: req, Failure: HeartbeatLeaseElapsed}, nil
	}
	if req.ClientLeaseUntil != nil && !databaseNow.Before(*req.ClientLeaseUntil) {
		return HeartbeatOutcome{Request: req, Failure: HeartbeatLeaseElapsed}, nil
	}
	if sessionID != "" {
		if req.DeviceSessionID != sessionID {
			return HeartbeatOutcome{Request: req, Failure: HeartbeatSessionEnded}, nil
		}
		var sessionCurrent bool
		if err := tx.QueryRow(ctx, `SELECT EXISTS (
			SELECT 1 FROM device_sessions s
			WHERE s.session_id = $1 AND NOT s.revoked AND s.expires_at > clock_timestamp()
			  AND (requests.device_session_authority IS NULL
			       OR s.authority_revision = requests.device_session_authority)
			  AND ($3 = '' OR s.authority_revision = $3))
			FROM requests WHERE requests.request_id = $2`, sessionID, requestID, servingDeviceSessionAuthority(ctx)).Scan(&sessionCurrent); err != nil {
			return HeartbeatOutcome{}, err
		}
		if !sessionCurrent {
			return HeartbeatOutcome{Request: req, Failure: HeartbeatSessionEnded}, nil
		}
	}
	updated, err := scanRequest(tx.QueryRow(ctx, `
		UPDATE requests
		SET client_lease_until = LEAST(expires_at,
		        GREATEST(COALESCE(client_lease_until, clock_timestamp()), clock_timestamp() + $3)),
		    updated_at = GREATEST(updated_at, clock_timestamp())
		WHERE request_id = $1 AND device_id = $2
		  AND client_lease_state = 'live'
		  AND status IN (`+nonTerminalStatuses+`)
		  AND expires_at > clock_timestamp()
		RETURNING `+requestColumns,
		requestID, deviceID, lease))
	if errors.Is(err, pgx.ErrNoRows) {
		// A concurrent expiry or terminal transition won after the row lock
		// was released. Renewal never retries the same decision.
		return HeartbeatOutcome{Request: req, Failure: HeartbeatLeaseElapsed}, nil
	}
	if err != nil {
		return HeartbeatOutcome{}, fmt.Errorf("heartbeat request: %w", err)
	}
	if err := tx.Commit(ctx); err != nil {
		return HeartbeatOutcome{}, err
	}
	return HeartbeatOutcome{Request: updated, Renewed: true}, nil
}

// ClaimNoticeIntents leases due notice intents with FOR UPDATE SKIP LOCKED.
// The transaction commits before publication; a crashed publisher is retried
// after the lease elapses, and duplicate notices are harmless.
func (s *Store) ClaimNoticeIntents(ctx context.Context, now time.Time, limit int, lease time.Duration) ([]Request, error) {
	if limit <= 0 {
		return nil, nil
	}
	rows, err := s.poolQuery(ctx, `
		WITH due AS (
			SELECT request_id FROM requests
			WHERE notice_pending
			  AND (notice_next_attempt IS NULL OR notice_next_attempt <= $1)
			ORDER BY COALESCE(notice_next_attempt, notice_updated_at), request_id
			LIMIT $2
			FOR UPDATE SKIP LOCKED
		)
		UPDATE requests r
		SET notice_next_attempt = $3,
		    notice_attempts = r.notice_attempts + 1,
		    updated_at = GREATEST(r.updated_at, $1)
		FROM due
		WHERE r.request_id = due.request_id
		RETURNING `+requestColumnsR, now, limit, now.Add(lease))
	if err != nil {
		return nil, fmt.Errorf("claim notice intents: %w", err)
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
		return nil, fmt.Errorf("claim notice intents: %w", err)
	}
	return out, nil
}

// MarkNoticePublished records a successful publication only when the claimed
// status is still the latest intent. A transition that arrived while the
// publication was in flight leaves notice_pending set for the newer state.
func (s *Store) MarkNoticePublished(ctx context.Context, requestID string, status whalereinv1.OnlineStatus, updatedAt time.Time, now time.Time) (bool, error) {
	cmd, err := s.poolExec(ctx, `
		UPDATE requests
		SET notice_pending = false,
		    notice_next_attempt = NULL,
		    notice_published_at = $4,
		    updated_at = GREATEST(updated_at, $4)
		WHERE request_id = $1 AND notice_pending
		  AND notice_status = $2
		  AND notice_updated_at IS NOT DISTINCT FROM $3`,
		requestID, StatusName(status), updatedAt, now)
	if err != nil {
		return false, fmt.Errorf("mark notice published: %w", err)
	}
	return cmd.RowsAffected() == 1, nil
}

// MarkNoticeFailure keeps the intent pending and schedules one bounded retry.
func (s *Store) MarkNoticeFailure(ctx context.Context, requestID string, now, nextAttempt time.Time) error {
	_, err := s.poolExec(ctx, `
		UPDATE requests
		SET notice_next_attempt = $2,
		    updated_at = GREATEST(updated_at, $3)
		WHERE request_id = $1 AND notice_pending`, requestID, nextAttempt, now)
	if err != nil {
		return fmt.Errorf("mark notice failure: %w", err)
	}
	return nil
}

// NoticeBacklog is the bounded operator snapshot for notice publication.
type NoticeBacklog struct {
	Pending        int
	Attempts       int64
	OldestAge      time.Duration
	PublishedTotal int64
}

// NoticeStats reads pending notice counts, accumulated attempts and the age of
// the oldest pending intent. It uses database time and never touches body or
// identity metadata.
func (s *Store) NoticeStats(ctx context.Context) (NoticeBacklog, error) {
	var pending int
	var attempts int64
	var oldestSeconds float64
	var published int64
	err := s.poolQueryRow(ctx, `
		SELECT COALESCE(count(*) FILTER (WHERE notice_pending), 0)::int,
		       COALESCE(sum(notice_attempts) FILTER (WHERE notice_pending), 0)::bigint,
		       COALESCE(EXTRACT(EPOCH FROM (clock_timestamp() - min(notice_updated_at) FILTER (WHERE notice_pending))), 0)::float8,
		       COALESCE(count(*) FILTER (WHERE notice_published_at IS NOT NULL), 0)::bigint
		FROM requests`).Scan(&pending, &attempts, &oldestSeconds, &published)
	if err != nil {
		return NoticeBacklog{}, fmt.Errorf("notice stats: %w", err)
	}
	return NoticeBacklog{
		Pending:        pending,
		Attempts:       attempts,
		OldestAge:      time.Duration(oldestSeconds * float64(time.Second)),
		PublishedTotal: published,
	}, nil
}
