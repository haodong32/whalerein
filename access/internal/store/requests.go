package store

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
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
	return createRequest(ctx, s.poolQueryRow, req)
}

func createRequest(ctx context.Context, queryRow func(context.Context, string, ...any) pgx.Row, req Request) (CreateOutcome, error) {
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
			INSERT INTO requests (` + requestColumns + `,forward_provenance_known)
			VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,true)
			ON CONFLICT (host_id, device_id, command_id) WHERE command_id <> '' DO NOTHING
			RETURNING ` + requestColumns
		row := queryRow(ctx, insertSQL, args...)
		inserted, err := scanRequest(row)
		switch {
		case err == nil:
			return CreateOutcome{Request: inserted, Created: true}, nil
		case errors.Is(err, pgx.ErrNoRows):
			existing, err := requestByCommand(ctx, queryRow, req.HostID, req.DeviceID, req.CommandID)
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
		INSERT INTO requests (` + requestColumns + `,forward_provenance_known)
		VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,true)
		RETURNING ` + requestColumns
	row := queryRow(ctx, insertSQL, args...)
	inserted, err := scanRequest(row)
	if err != nil {
		if isUniqueViolation(err) {
			return CreateOutcome{}, fmt.Errorf("create request: %w", ErrConflict)
		}
		return CreateOutcome{}, fmt.Errorf("create request: %w", err)
	}
	return CreateOutcome{Request: inserted, Created: true}, nil
}

func requestByCommand(ctx context.Context, queryRow func(context.Context, string, ...any) pgx.Row, hostID, deviceID, commandID string) (Request, error) {
	row := queryRow(ctx, `
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
	row := s.db.QueryRow(ctx, `SELECT `+requestColumns+` FROM requests WHERE request_id = $1`, requestID)
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
	rows, err := s.db.Query(ctx, query, now, limit, now.Add(lease))
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
	cmd, err := s.db.Exec(ctx, `
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
	_, err := s.db.Exec(ctx, `
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

// MarkPublishFailure schedules another attempt or leaves an unconfirmed
// publication unknown after the attempt budget. A row known to have been
// published stays received until expiry and can still accept a Host result.
func (s *Store) MarkPublishFailure(ctx context.Context, requestID string, now time.Time, retryAfter time.Duration, maxAttempts int) error {
	_, err := s.db.Exec(ctx, `
		UPDATE requests
		SET status = CASE
                WHEN kind = 5 THEN status
		        WHEN forward_attempts >= $2 AND NOT published_once THEN 'unknown'
		        ELSE status END,
		    error_code = CASE
		        WHEN kind <> 5 AND forward_attempts >= $2 AND NOT published_once THEN $5
		        ELSE error_code END,
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
		requestID, maxAttempts, now.Add(retryAfter), now, int32(whalereinv1.ErrorCode_unavailable))
	if err != nil {
		return fmt.Errorf("mark request publish failure: %w", err)
	}
	return nil
}

// statusTransitions is the single durable HostResult status matrix. It is both
// the predicate the store applies and the table the unit tests exercise, so a
// widened rollback (for example host_accepted back to rejected or host_offline)
// fails a test before it can reach SQL.
var statusTransitions = map[whalereinv1.OnlineStatus][]whalereinv1.OnlineStatus{
	whalereinv1.OnlineStatus_received: {
		whalereinv1.OnlineStatus_host_offline,
		whalereinv1.OnlineStatus_rejected,
		whalereinv1.OnlineStatus_host_accepted,
		whalereinv1.OnlineStatus_succeeded,
		whalereinv1.OnlineStatus_failed,
		whalereinv1.OnlineStatus_unknown,
	},
	whalereinv1.OnlineStatus_host_accepted: {
		whalereinv1.OnlineStatus_succeeded,
		whalereinv1.OnlineStatus_failed,
		whalereinv1.OnlineStatus_unknown,
	},
	whalereinv1.OnlineStatus_unknown: {
		whalereinv1.OnlineStatus_succeeded,
		whalereinv1.OnlineStatus_failed,
	},
}

// statusTransitionSources fixes the predicate order for stable SQL and tests.
var statusTransitionSources = []whalereinv1.OnlineStatus{
	whalereinv1.OnlineStatus_received,
	whalereinv1.OnlineStatus_host_accepted,
	whalereinv1.OnlineStatus_unknown,
}

// hostResultStatusPredicate is generated from statusTransitions and embedded in
// the applyHostResult UPDATE. There is no second hand-written SQL matrix.
var hostResultStatusPredicate = buildStatusPredicate(statusTransitions, statusTransitionSources)

func buildStatusPredicate(transitions map[whalereinv1.OnlineStatus][]whalereinv1.OnlineStatus, sources []whalereinv1.OnlineStatus) string {
	parts := make([]string, 0, len(sources))
	for _, from := range sources {
		targets := transitions[from]
		if len(targets) == 0 {
			continue
		}
		quoted := make([]string, len(targets))
		for index, target := range targets {
			quoted[index] = "'" + StatusName(target) + "'"
		}
		parts = append(parts, fmt.Sprintf("(status = '%s' AND $4 IN (%s))", StatusName(from), strings.Join(quoted, ",")))
	}
	return strings.Join(parts, " OR ")
}

// statusTransitionAllowed reports the matrix decision for tests and diagnostics.
func statusTransitionAllowed(from, to whalereinv1.OnlineStatus) bool {
	for _, allowed := range statusTransitions[from] {
		if allowed == to {
			return true
		}
	}
	return false
}

// ApplyHostResult conditionally updates the same request row. The WHERE clause
// encodes exactly the allowed online status transitions.
func (s *Store) ApplyHostResult(ctx context.Context, update HostResultUpdate) (bool, error) {
	return applyHostResult(ctx, s.poolBegin, update)
}

// applyHostResult atomically records the Host result and any valid pairing acknowledgement.
func applyHostResult(ctx context.Context, begin func(context.Context) (pgx.Tx, error), update HostResultUpdate) (bool, error) {
	if !validStatus(update.Status) {
		return false, fmt.Errorf("apply host result: invalid status %q", update.Status)
	}
	if update.TransportPreflight && (update.Status != whalereinv1.OnlineStatus_rejected ||
		(update.ErrorCode != whalereinv1.ErrorCode_protocol_version_incompatible && update.ErrorCode != whalereinv1.ErrorCode_payload_too_large)) {
		return false, errors.New("apply host result: invalid transport preflight")
	}
	if update.ResultBody == nil {
		update.ResultBody = []byte{}
	}
	var acknowledgement whalereinv1.PairingClaimResult
	candidate := update.Status == whalereinv1.OnlineStatus_succeeded && proto.Unmarshal(update.ResultBody, &acknowledgement) == nil && acknowledgement.Authorized && acknowledgement.DeviceId == update.DeviceID
	tx, err := begin(ctx)
	if err != nil {
		return false, err
	}
	defer func() { _ = tx.Rollback(ctx) }()
	if candidate {
		if _, err = tx.Exec(ctx, `SELECT pg_advisory_xact_lock(hashtextextended('whalerein-device:' || $1, 0))`, update.DeviceID); err != nil {
			return false, err
		}
	}
	// Acquire the request row after the device fence. Sampling time in an
	// UPDATE expression is insufficient: PostgreSQL may evaluate it before
	// waiting for the row lock.
	var expiresAt time.Time
	var bodiesCleared, legacyRetentionUnknown bool
	var requestBodyLength int64
	err = tx.QueryRow(ctx, `SELECT expires_at,COALESCE(bodies_cleared,false),bodies_cleared IS NULL,octet_length(request_body) FROM requests WHERE request_id=$1 AND host_id=$2 AND device_id=$3 FOR UPDATE`, update.RequestID, update.HostID, update.DeviceID).Scan(&expiresAt, &bodiesCleared, &legacyRetentionUnknown, &requestBodyLength)
	if errors.Is(err, pgx.ErrNoRows) {
		return false, nil
	}
	if err != nil {
		return false, fmt.Errorf("lock request for host result: %w", err)
	}
	var databaseNow time.Time
	if err = tx.QueryRow(ctx, `SELECT clock_timestamp()`).Scan(&databaseNow); err != nil {
		return false, err
	}
	applicationNow := time.Now().UTC()
	// Old empty rows cannot prove whether cleanup already occurred. A nonempty
	// legacy request body is safe: only cleanup clears it, and no path restores it.
	retentionKnown := !legacyRetentionUnknown || requestBodyLength > 0
	retainBody := !bodiesCleared && retentionKnown && update.At.Before(expiresAt) && applicationNow.Before(expiresAt) && databaseNow.Before(expiresAt)
	cmd, err := tx.Exec(ctx, `
		UPDATE requests
		SET status = CASE WHEN $13 AND (NOT forward_provenance_known OR published_once OR forward_attempts <> 1) THEN 'unknown' ELSE $4 END,
		    result_body = CASE WHEN $10 THEN $5 ELSE result_body END,
		    error_code = CASE WHEN $13 AND (NOT forward_provenance_known OR published_once OR forward_attempts <> 1) THEN $14::integer ELSE $6::integer END,
		    error_message = $7,
		    channel_id = CASE WHEN $8 <> '' THEN $8 ELSE channel_id END,
		    forward_state = 'none',
		    updated_at = GREATEST(updated_at,$9,$11,$12)
		WHERE request_id = $1 AND host_id = $2 AND device_id = $3
		  AND (`+hostResultStatusPredicate+`)`,
		update.RequestID,
		update.HostID,
		update.DeviceID,
		StatusName(update.Status),
		update.ResultBody,
		int32(update.ErrorCode),
		update.ErrorMessage,
		update.ChannelID,
		update.At,
		retainBody,
		applicationNow,
		databaseNow,
		update.TransportPreflight,
		int32(whalereinv1.ErrorCode_unavailable),
	)
	if err != nil {
		return false, fmt.Errorf("apply host result: %w", err)
	}
	applied := cmd.RowsAffected() == 1
	if applied && candidate {
		var claimID string
		err = tx.QueryRow(ctx, `SELECT o.claim_id FROM pairing_outcomes o JOIN requests r ON r.request_id=o.commit_request_id
          WHERE o.commit_request_id=$1 AND o.host_id=$2 AND o.device_id=$3 AND r.kind=$4 AND r.expires_at>$5`, update.RequestID, update.HostID, update.DeviceID, int32(whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE), update.At).Scan(&claimID)
		if err == nil {
			if _, err = confirmPairingHostTx(ctx, tx, claimID, update.HostID, update.DeviceID, update.RequestID, update.At, true); err != nil {
				return false, err
			}
		} else if !errors.Is(err, pgx.ErrNoRows) {
			return false, err
		}
	}
	if err = tx.Commit(ctx); err != nil {
		return false, err
	}
	return applied, nil
}

// ClearExpiredBodies drops request and result bodies after expiry while
// preserving the routing identity and terminal status copy.
func (s *Store) ClearExpiredBodies(ctx context.Context, now time.Time) (int64, error) {
	if err := s.reconcilePairingOutcomes(ctx, now, false); err != nil {
		return 0, fmt.Errorf("reconcile pairing outcomes: %w", err)
	}
	// Bounded so one cleanup pass cannot hold a long lock after a long pause;
	// the caller repeats until a pass returns fewer than CleanupBatchSize rows.
	cmd, err := s.db.Exec(ctx, `
		UPDATE requests
		SET request_body = ''::bytea, result_body = ''::bytea, bodies_cleared=true, updated_at = GREATEST(updated_at,$1)
		WHERE request_id IN (
			SELECT request_id FROM requests
			WHERE expires_at <= $1
			  AND (request_body <> ''::bytea OR result_body <> ''::bytea OR bodies_cleared IS DISTINCT FROM true)
			ORDER BY expires_at, request_id
			LIMIT $2)`, now, CleanupBatchSize)
	if err != nil {
		return 0, fmt.Errorf("clear expired request bodies: %w", err)
	}
	return cmd.RowsAffected(), nil
}

// CountRequests is a component-test helper.
func (s *Store) CountRequests(ctx context.Context) (int, error) {
	var count int
	if err := s.db.QueryRow(ctx, `SELECT count(*) FROM requests`).Scan(&count); err != nil {
		return 0, fmt.Errorf("count requests: %w", err)
	}
	return count, nil
}

// DeleteExpiredQueries removes a bounded batch of old, non-control records.
// Command and activation identities remain available for dedupe and recovery.
func (s *Store) DeleteExpiredQueries(ctx context.Context, before time.Time) (int64, error) {
	result, err := s.db.Exec(ctx, `DELETE FROM requests WHERE request_id IN (
        SELECT request_id FROM requests WHERE expires_at <= $1 AND command_id=''
        AND kind IN (1,2) ORDER BY expires_at,request_id LIMIT 2000)`, before)
	if err != nil {
		return 0, fmt.Errorf("delete expired query records: %w", err)
	}
	return result.RowsAffected(), nil
}
