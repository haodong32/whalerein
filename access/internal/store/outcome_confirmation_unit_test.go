package store

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"strings"
	"testing"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgconn"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

type confirmTestRow struct {
	values []any
	err    error
}

func (r confirmTestRow) Scan(dest ...any) error {
	if r.err != nil {
		return r.err
	}
	if len(dest) != len(r.values) {
		panic("unexpected scan shape")
	}
	for i, v := range r.values {
		switch x := v.(type) {
		case bool:
			*dest[i].(*bool) = x
		case []byte:
			*dest[i].(*[]byte) = x
		case uint32:
			*dest[i].(*uint32) = x
		case uint64:
			*dest[i].(*uint64) = x
		case int64:
			*dest[i].(*int64) = x
		case time.Time:
			*dest[i].(*time.Time) = x
		case string:
			*dest[i].(*string) = x
		default:
			panic("unexpected scan value")
		}
	}
	return nil
}

type confirmTestTx struct {
	pgx.Tx
	t     *testing.T
	mode  string
	at    time.Time
	key   []byte
	steps []string
}

func (tx *confirmTestTx) QueryRow(_ context.Context, sql string, args ...any) pgx.Row {
	step := "fingerprint"
	var values []any
	switch {
	case strings.Contains(sql, "SELECT EXISTS(SELECT 1 FROM requests"):
		step = "request"
		if len(args) != 5 || args[0] != "request" || args[1] != "host" || args[2] != "device" || args[4] != tx.at {
			tx.t.Fatal("request scope or deadline changed")
		}
		values = []any{tx.mode != "request-missing"}
	case strings.Contains(sql, "SELECT o.device_public_key"):
		step = "outcome"
		if len(args) != 4 || args[0] != "claim" || args[1] != "host" || args[2] != "device" || args[3] != "request" {
			tx.t.Fatal("outcome scope changed")
		}
		key := tx.key
		protocol := uint32(1)
		active := tx.mode != "inactive"
		if strings.HasPrefix(tx.mode, "legacy") {
			key = nil
			protocol = 0
		}
		if tx.mode == "short-key" {
			key = make([]byte, 31)
		}
		if tx.mode == "zero-protocol" {
			protocol = 0
		}
		values = []any{key, protocol, active, true}
	case strings.Contains(sql, "SELECT p.finish_response"):
		step = "legacy"
		response := &whalereinv1.PairingClaimResponse{ClaimId: "claim", HostId: "host", DeviceId: "device", Phase: whalereinv1.PairingPhase_PAIRING_PHASE_FINISH, DevicePublicKey: tx.key, ProtocolVersion: 1}
		if tx.mode == "legacy-mismatch" {
			response.DeviceId = "other"
		}
		encoded, _ := proto.Marshal(response)
		values = []any{encoded}
	case strings.Contains(sql, "SELECT r.request_body,o.grant_generation"):
		step = "binding"
		body, _ := proto.Marshal(&whalereinv1.PairingClaim{ClaimId: "claim", DeviceId: "device", Phase: whalereinv1.PairingPhase_PAIRING_PHASE_COMMIT, DevicePublicKey: tx.key, GrantGeneration: 1, ExpiresAtUnixMs: tx.at.Add(time.Minute).UnixMilli()})
		hash := sha256.Sum256(tx.key)
		values = []any{body, uint64(1), hex.EncodeToString(hash[:]), uint32(1), tx.at, tx.at.Add(time.Minute)}
	default:
		if len(args) != 1 || args[0] != "device" {
			tx.t.Fatal("fingerprint scope changed")
		}
		hash := sha256.Sum256(tx.key)
		fingerprint := hex.EncodeToString(hash[:])
		if tx.mode == "fingerprint-mismatch" {
			fingerprint = "other"
		}
		values = []any{fingerprint}
	}
	tx.steps = append(tx.steps, step)
	if tx.mode == step+"-error" {
		return confirmTestRow{err: errStoreUnit}
	}
	if tx.mode == step+"-missing" && step != "request" {
		return confirmTestRow{err: pgx.ErrNoRows}
	}
	return confirmTestRow{values: values}
}
func (tx *confirmTestTx) Exec(_ context.Context, sql string, args ...any) (pgconn.CommandTag, error) {
	if strings.Contains(sql, "pg_advisory_xact_lock") {
		tx.steps = append(tx.steps, "lock")
		if len(args) != 1 || args[0] != "device" {
			tx.t.Fatal("wrong device fence")
		}
		if tx.mode == "lock-error" {
			return pgconn.CommandTag{}, errStoreUnit
		}
		return pgconn.NewCommandTag("SELECT 1"), nil
	}
	tx.steps = append(tx.steps, "update")
	if !strings.Contains(sql, "UPDATE pairing_outcomes") || len(args) != 7 || args[0] != "claim" || args[1] != "host" || args[2] != "device" || args[3] != "request" {
		tx.t.Fatal("confirmation write scope changed")
	}
	if tx.mode == "update-error" {
		return pgconn.CommandTag{}, errStoreUnit
	}
	if tx.mode == "update-missing" {
		return pgconn.NewCommandTag("UPDATE 0"), nil
	}
	return pgconn.NewCommandTag("UPDATE 1"), nil
}
func TestConfirmPairingHostRejectsExpiredInactiveAndMismatchedEvidence(t *testing.T) {
	at := time.Unix(100, 0)
	key := make([]byte, 32)
	for _, tc := range []struct {
		name    string
		want    bool
		wantErr error
	}{
		{"valid", true, nil}, {"legacy-valid", true, nil}, {"request-missing", false, ErrConflict}, {"request-error", false, errStoreUnit}, {"outcome-missing", false, ErrNotFound}, {"outcome-error", false, errStoreUnit}, {"inactive", false, nil}, {"short-key", false, ErrConflict}, {"zero-protocol", false, ErrConflict}, {"legacy-mismatch", false, ErrConflict}, {"legacy-missing", false, ErrNotFound}, {"fingerprint-mismatch", false, ErrConflict}, {"fingerprint-missing", false, ErrNotFound}, {"update-error", false, errStoreUnit}, {"update-missing", false, ErrConflict},
	} {
		t.Run(tc.name, func(t *testing.T) {
			tx := &confirmTestTx{t: t, mode: tc.name, at: at, key: key}
			got, err := confirmPairingHostTx(context.Background(), tx, "claim", "host", "device", "request", at, true)
			if got != tc.want || !errors.Is(err, tc.wantErr) {
				t.Fatalf("confirmed=%v err=%v steps=%v", got, err, tx.steps)
			}
			if !tc.want {
				for _, step := range tx.steps {
					if step == "update" && tc.name != "update-error" && tc.name != "update-missing" {
						t.Fatal("invalid evidence was updated")
					}
				}
			}
		})
	}
}

type outcomeObservationRow struct {
	tx   *outcomeObservationTx
	step string
	err  error
}

func (r outcomeObservationRow) Scan(d ...any) error {
	if r.err != nil {
		return r.err
	}
	tx := r.tx
	switch r.step {
	case "load":
		*d[0].(*string) = "claim"
		*d[1].(*[]byte) = []byte{1}
		*d[2].(*string) = "host"
		*d[3].(*string) = "device"
		*d[4].(*[]byte) = []byte{2}
		*d[5].(*[]byte) = tx.key
		*d[6].(*uint32) = 1
		*d[7].(*bool) = tx.confirmed
		*d[8].(*string) = "request"
		*d[9].(*bool) = !tx.unverified
		*d[10].(*bool) = !tx.confirmationUnverified
		*d[11].(*bool) = tx.active
	case "ack":
		*d[0].(*[]byte) = tx.ack
		*d[1].(*time.Time) = tx.at
	case "request":
		*d[0].(*bool) = true
	case "outcome":
		*d[0].(*[]byte) = tx.key
		*d[1].(*uint32) = 1
		*d[2].(*bool) = tx.active
		*d[3].(*bool) = !tx.unverified
	case "fingerprint":
		hash := sha256.Sum256(tx.key)
		*d[0].(*string) = hex.EncodeToString(hash[:])
	case "binding":
		body, _ := proto.Marshal(&whalereinv1.PairingClaim{ClaimId: "claim", DeviceId: "device", Phase: whalereinv1.PairingPhase_PAIRING_PHASE_COMMIT, DevicePublicKey: tx.key, GrantGeneration: 1, ExpiresAtUnixMs: tx.at.Add(time.Minute).UnixMilli()})
		hash := sha256.Sum256(tx.key)
		*d[0].(*[]byte) = body
		*d[1].(*uint64) = 1
		*d[2].(*string) = hex.EncodeToString(hash[:])
		*d[3].(*uint32) = 1
		*d[4].(*time.Time) = tx.at
		*d[5].(*time.Time) = tx.at.Add(time.Minute)
	}
	return nil
}

type outcomeObservationTx struct {
	pgx.Tx
	t                                  *testing.T
	active, confirmed                  bool
	unverified, confirmationUnverified bool
	key, ack                           []byte
	at                                 time.Time
	fail                               string
	steps                              []string
}

func (tx *outcomeObservationTx) Exec(_ context.Context, sql string, args ...any) (pgconn.CommandTag, error) {
	step := "lock"
	if strings.Contains(sql, "SET commit_binding_verified=true WHERE") {
		step = "proof"
		if len(args) != 4 || args[0] != "claim" || args[1] != "host" || args[2] != "device" || args[3] != "request" {
			tx.t.Fatal("wrong proof scope")
		}
		if tx.fail == "proof-missing" {
			return pgconn.NewCommandTag("UPDATE 0"), nil
		}
		if tx.fail == "proof" {
			return pgconn.CommandTag{}, errStoreUnit
		}
		tx.unverified = false
	} else if strings.Contains(sql, "SET host_confirmation_verified=true WHERE") {
		step = "ack-proof"
		if len(args) != 4 || args[0] != "claim" || args[1] != "host" || args[2] != "device" || args[3] != "request" {
			tx.t.Fatal("wrong acknowledgement proof scope")
		}
		if tx.fail == "ack-proof-missing" {
			return pgconn.NewCommandTag("UPDATE 0"), nil
		}
		if tx.fail == "ack-proof" {
			return pgconn.CommandTag{}, errStoreUnit
		}
		tx.confirmationUnverified = false
	} else if strings.Contains(sql, "UPDATE pairing_outcomes") {
		step = "confirm"
		if len(args) != 7 || args[0] != "claim" || args[1] != "host" || args[2] != "device" || args[3] != "request" {
			tx.t.Fatal("wrong confirmation scope")
		}
		tx.confirmed = true
		tx.unverified = false
		tx.confirmationUnverified = false
	} else if len(args) != 1 || args[0] != "device" {
		tx.t.Fatal("wrong device lock")
	}
	tx.steps = append(tx.steps, step)
	if tx.fail == step {
		return pgconn.CommandTag{}, errStoreUnit
	}
	return pgconn.NewCommandTag("UPDATE 1"), nil
}

func TestLegacyProofPersistenceFailsClosed(t *testing.T) {
	for _, tc := range []struct {
		name, fail string
		wantErr    error
	}{
		{"proof write succeeds", "", nil},
		{"proof write fails", "proof", errStoreUnit},
		{"outcome disappeared", "proof-missing", ErrConflict},
		{"ack proof write fails", "ack-proof", errStoreUnit},
		{"ack proof row disappeared", "ack-proof-missing", ErrConflict},
	} {
		t.Run(tc.name, func(t *testing.T) {
			ack, _ := proto.Marshal(&whalereinv1.PairingClaimResult{Authorized: true, DeviceId: "device"})
			tx := &outcomeObservationTx{t: t, active: true, confirmed: true, unverified: true, confirmationUnverified: true, key: make([]byte, 32), ack: ack, at: time.Unix(100, 0), fail: tc.fail}
			out, err := pairingOutcome(context.Background(), func(context.Context) (pgx.Tx, error) { return tx, nil }, "claim", "device")
			if !errors.Is(err, tc.wantErr) {
				t.Fatalf("unexpected proof error: %v", err)
			}
			if tc.wantErr == nil && (!out.HostConfirmed || out.CommitBindingUnproven) {
				t.Fatal("verified legacy proof did not become observable")
			}
			for _, step := range tx.steps {
				if tc.wantErr != nil && step == "commit" {
					t.Fatal("failed proof write committed")
				}
			}
			if tx.steps[len(tx.steps)-1] != "rollback" {
				t.Fatal("proof transaction was not released")
			}
		})
	}
}
func (tx *outcomeObservationTx) QueryRow(_ context.Context, sql string, args ...any) pgx.Row {
	step := "fingerprint"
	switch {
	case strings.Contains(sql, "SELECT o.claim_id"):
		step = "load"
		if len(args) != 2 || args[0] != "claim" || args[1] != "device" {
			tx.t.Fatal("wrong outcome identity")
		}
	case strings.Contains(sql, "SELECT result_body"):
		step = "ack"
		if len(args) != 4 || args[0] != "request" || args[1] != "host" || args[2] != "device" {
			tx.t.Fatal("wrong acknowledgement scope")
		}
	case strings.Contains(sql, "SELECT EXISTS(SELECT 1 FROM requests"):
		step = "request"
	case strings.Contains(sql, "SELECT o.device_public_key"):
		step = "outcome"
	case strings.Contains(sql, "SELECT r.request_body,o.grant_generation"):
		step = "binding"
	}
	tx.steps = append(tx.steps, step)
	if tx.fail == step {
		return outcomeObservationRow{err: errStoreUnit}
	}
	if tx.fail == "ack-missing" && step == "ack" {
		return outcomeObservationRow{err: pgx.ErrNoRows}
	}
	return outcomeObservationRow{tx: tx, step: step}
}
func (tx *outcomeObservationTx) Commit(context.Context) error {
	tx.steps = append(tx.steps, "commit")
	if tx.fail == "commit" {
		return errStoreUnit
	}
	return nil
}
func (tx *outcomeObservationTx) Rollback(context.Context) error {
	tx.steps = append(tx.steps, "rollback")
	return nil
}
func TestPairingOutcomeRecoversOnlyValidScopedLegacyAcknowledgement(t *testing.T) {
	for _, tc := range []struct {
		name, fail                                 string
		active, confirmed, validAck, wantConfirmed bool
		wantErr                                    error
	}{
		{"inactive", "", false, false, false, false, nil}, {"already confirmed", "", true, true, false, true, nil}, {"missing acknowledgement", "ack-missing", true, false, false, false, nil}, {"invalid acknowledgement", "", true, false, false, false, nil}, {"valid legacy acknowledgement", "", true, false, true, true, nil}, {"lock failure", "lock", true, false, true, false, errStoreUnit}, {"load failure", "load", true, false, true, false, errStoreUnit}, {"ack read failure", "ack", true, false, true, false, errStoreUnit}, {"confirm failure", "confirm", true, false, true, false, errStoreUnit}, {"commit failure", "commit", true, true, false, false, errStoreUnit},
	} {
		t.Run(tc.name, func(t *testing.T) {
			ack := []byte{0xff}
			if tc.validAck {
				ack, _ = proto.Marshal(&whalereinv1.PairingClaimResult{Authorized: true, DeviceId: "device"})
			}
			tx := &outcomeObservationTx{t: t, active: tc.active, confirmed: tc.confirmed, key: make([]byte, 32), ack: ack, at: time.Unix(100, 0), fail: tc.fail}
			value, err := pairingOutcome(context.Background(), func(context.Context) (pgx.Tx, error) { return tx, nil }, "claim", "device")
			if !errors.Is(err, tc.wantErr) || (tc.wantErr == nil && value.HostConfirmed != tc.wantConfirmed) {
				t.Fatalf("err=%v confirmed=%v steps=%v", err, value.HostConfirmed, tx.steps)
			}
			if tx.steps[len(tx.steps)-1] != "rollback" {
				t.Fatal("transaction not released")
			}
		})
	}
}

func (tx *confirmTestTx) Commit(context.Context) error {
	tx.steps = append(tx.steps, "commit")
	if tx.mode == "commit-error" {
		return errStoreUnit
	}
	return nil
}
func (tx *confirmTestTx) Rollback(context.Context) error {
	tx.steps = append(tx.steps, "rollback")
	return nil
}
func TestConfirmPairingHostClosesTransactionAndCommitsOnlyValidEvidence(t *testing.T) {
	at := time.Unix(100, 0)
	for _, tc := range []struct {
		name    string
		want    bool
		wantErr error
	}{
		{"valid", true, nil}, {"inactive", false, nil}, {"lock-error", false, errStoreUnit}, {"outcome-missing", false, ErrNotFound}, {"commit-error", false, errStoreUnit},
	} {
		t.Run(tc.name, func(t *testing.T) {
			tx := &confirmTestTx{t: t, mode: tc.name, at: at, key: make([]byte, 32)}
			got, err := confirmPairingHost(context.Background(), func(context.Context) (pgx.Tx, error) { return tx, nil }, "claim", "host", "device", "request", at)
			if got != tc.want || !errors.Is(err, tc.wantErr) {
				t.Fatalf("confirmed=%v err=%v steps=%v", got, err, tx.steps)
			}
			if tx.steps[len(tx.steps)-1] != "rollback" {
				t.Fatal("transaction not released")
			}
		})
	}
}

type hostResultTx struct {
	confirmTestTx
	hostMode string
	affected int
}

func (tx *hostResultTx) Exec(ctx context.Context, sql string, args ...any) (pgconn.CommandTag, error) {
	if strings.Contains(sql, "UPDATE requests") {
		tx.steps = append(tx.steps, "request-update")
		if len(args) != 14 || args[0] != "request" || args[1] != "host" || args[2] != "device" || args[7] != "channel" || args[8] != tx.at {
			tx.t.Fatal("host result scope changed")
		}
		if tx.hostMode == "update-error" {
			return pgconn.CommandTag{}, errStoreUnit
		}
		return pgconn.NewCommandTag(fmt.Sprintf("UPDATE %d", tx.affected)), nil
	}
	return tx.confirmTestTx.Exec(ctx, sql, args...)
}
func (tx *hostResultTx) QueryRow(ctx context.Context, sql string, args ...any) pgx.Row {
	if strings.Contains(sql, "SELECT expires_at,COALESCE(bodies_cleared") {
		tx.steps = append(tx.steps, "request-lock")
		if tx.hostMode == "request-lock-error" {
			return confirmTestRow{err: errStoreUnit}
		}
		if tx.hostMode == "request-lock-missing" {
			return confirmTestRow{err: pgx.ErrNoRows}
		}
		return confirmTestRow{values: []any{time.Now().Add(time.Hour), false, false, int64(1)}}
	}
	if strings.Contains(sql, "SELECT clock_timestamp()") {
		tx.steps = append(tx.steps, "database-clock")
		if tx.hostMode == "clock-error" {
			return confirmTestRow{err: errStoreUnit}
		}
		return confirmTestRow{values: []any{time.Now()}}
	}
	if strings.Contains(sql, "SELECT o.claim_id FROM pairing_outcomes") {
		tx.steps = append(tx.steps, "find-outcome")
		if len(args) != 5 || args[0] != "request" || args[1] != "host" || args[2] != "device" || args[4] != tx.at {
			tx.t.Fatal("outcome lookup scope changed")
		}
		if tx.hostMode == "lookup-error" {
			return confirmTestRow{err: errStoreUnit}
		}
		if tx.hostMode == "no-outcome" {
			return confirmTestRow{err: pgx.ErrNoRows}
		}
		return confirmTestRow{values: []any{"claim"}}
	}
	return tx.confirmTestTx.QueryRow(ctx, sql, args...)
}
func TestHostResultConfirmsOnlyAValidScopedAcknowledgementInSameTransaction(t *testing.T) {
	at := time.Unix(100, 0)
	for _, tc := range []struct {
		name                      string
		ack, authorized, matching bool
		affected                  int
		want                      bool
		wantErr                   error
	}{
		{"ordinary result", false, false, false, 1, true, nil}, {"valid acknowledgement", true, true, true, 1, true, nil}, {"no-outcome", true, true, true, 1, true, nil}, {"wrong device acknowledgement", true, true, false, 1, true, nil}, {"not authorized acknowledgement", true, false, true, 1, true, nil}, {"unmatched request", true, true, true, 0, false, nil}, {"lock-error", true, true, true, 1, false, errStoreUnit}, {"request-lock-error", true, true, true, 1, false, errStoreUnit}, {"request-lock-missing", true, true, true, 1, false, nil}, {"clock-error", true, true, true, 1, false, errStoreUnit}, {"update-error", true, true, true, 1, false, errStoreUnit}, {"lookup-error", true, true, true, 1, false, errStoreUnit}, {"commit-error", false, false, false, 1, false, errStoreUnit},
	} {
		t.Run(tc.name, func(t *testing.T) {
			body := []byte{0xff}
			if tc.ack {
				device := "device"
				if !tc.matching {
					device = "other"
				}
				body, _ = proto.Marshal(&whalereinv1.PairingClaimResult{Authorized: tc.authorized, DeviceId: device})
			}
			tx := &hostResultTx{confirmTestTx: confirmTestTx{t: t, mode: tc.name, at: at, key: make([]byte, 32)}, hostMode: tc.name, affected: tc.affected}
			update := HostResultUpdate{RequestID: "request", HostID: "host", DeviceID: "device", ChannelID: "channel", Status: whalereinv1.OnlineStatus_succeeded, ResultBody: body, At: at}
			applied, err := applyHostResult(context.Background(), func(context.Context) (pgx.Tx, error) { return tx, nil }, update)
			if applied != tc.want || !errors.Is(err, tc.wantErr) {
				t.Fatalf("applied=%v err=%v steps=%v", applied, err, tx.steps)
			}
			if tx.steps[len(tx.steps)-1] != "rollback" {
				t.Fatal("transaction not released")
			}
			if !tc.ack || !tc.authorized || !tc.matching {
				for _, step := range tx.steps {
					if step == "lock" || step == "find-outcome" {
						t.Fatal("ordinary result triggered pairing confirmation")
					}
				}
			}
		})
	}
}

type reconciliationEntry struct {
	claim, host, device, request string
	created                      time.Time
	binding                      bool
}
type reconciliationRows struct {
	pgx.Rows
	items  []reconciliationEntry
	index  int
	fail   string
	closed bool
}

func (r *reconciliationRows) Next() bool { r.index++; return r.index <= len(r.items) }
func (r *reconciliationRows) Scan(d ...any) error {
	if r.fail == "scan" {
		return errStoreUnit
	}
	v := r.items[r.index-1]
	*d[0].(*string) = v.claim
	*d[1].(*string) = v.host
	*d[2].(*string) = v.device
	*d[3].(*string) = v.request
	*d[4].(*time.Time) = v.created
	*d[5].(*bool) = v.binding
	return nil
}
func (r *reconciliationRows) Close() { r.closed = true }
func (r *reconciliationRows) Err() error {
	if r.fail == "rows" {
		return errStoreUnit
	}
	return nil
}
func TestReconciliationProcessesAllBatchesBeforeRetentionCleanup(t *testing.T) {
	at := time.Unix(100, 0)
	pages := 0
	observed := 0
	closed := 0
	query := func(_ context.Context, _ string, args ...any) (pgx.Rows, error) {
		pages++
		if len(args) != 4 || args[0] != at || args[1] != true || args[3] != 256 {
			t.Fatal("cleanup scope changed")
		}
		rows := &reconciliationRows{}
		switch pages {
		case 1:
			if args[2] != "" {
				t.Fatal("initial cursor changed")
			}
			for i := 0; i < 256; i++ {
				rows.items = append(rows.items, reconciliationEntry{claim: fmt.Sprintf("claim-%03d", i), host: "host", device: "device", request: "request", created: at})
			}
		case 2:
			if args[2] != "claim-255" {
				t.Fatal("next batch cursor changed")
			}
			rows.items = []reconciliationEntry{{claim: "claim-256", host: "host", device: "device", request: "request", created: at}}
		default:
			t.Fatal("unexpected extra batch")
		}
		return &closeCountingRows{reconciliationRows: rows, onClose: func() { closed++ }}, nil
	}
	begin := func(context.Context) (pgx.Tx, error) {
		t.Fatal("bound outcomes should not start legacy binding transaction")
		return nil, nil
	}
	observe := func(_ context.Context, claim, device string) (PairingOutcome, error) {
		if claim == "" || device != "device" {
			t.Fatal("outcome scope changed")
		}
		observed++
		return PairingOutcome{}, nil
	}
	if err := reconcilePairingOutcomes(context.Background(), query, begin, observe, at, true); err != nil || pages != 2 || observed != 257 || closed != 2 {
		t.Fatalf("err=%v pages=%d observed=%d closed=%d", err, pages, observed, closed)
	}
}

type closeCountingRows struct {
	*reconciliationRows
	onClose func()
}

func (r *closeCountingRows) Close() { r.reconciliationRows.Close(); r.onClose() }
func TestReconciliationFailsClosedOnStorageAndOutcomeErrors(t *testing.T) {
	at := time.Unix(100, 0)
	for _, mode := range []string{"query", "scan", "rows", "observe", "missing-is-safe"} {
		t.Run(mode, func(t *testing.T) {
			rows := &reconciliationRows{items: []reconciliationEntry{{claim: "claim", host: "host", device: "device", request: "request", created: at}}, fail: mode}
			query := func(context.Context, string, ...any) (pgx.Rows, error) {
				if mode == "query" {
					return nil, errStoreUnit
				}
				return rows, nil
			}
			begin := func(context.Context) (pgx.Tx, error) { t.Fatal("unexpected binding transaction"); return nil, nil }
			observe := func(context.Context, string, string) (PairingOutcome, error) {
				if mode == "observe" {
					return PairingOutcome{}, errStoreUnit
				}
				if mode == "missing-is-safe" {
					return PairingOutcome{}, ErrNotFound
				}
				return PairingOutcome{}, nil
			}
			err := reconcilePairingOutcomes(context.Background(), query, begin, observe, at, false)
			if mode == "missing-is-safe" {
				if err != nil {
					t.Fatal(err)
				}
			} else if !errors.Is(err, errStoreUnit) {
				t.Fatalf("expected failure: %v", err)
			}
			if mode != "query" && !rows.closed {
				t.Fatal("rows not closed")
			}
		})
	}
}

func TestLegacyReconciliationBindsBeforeObservationAndPreservesFailures(t *testing.T) {
	at := time.Unix(100, 0)
	for _, tc := range []struct {
		name, fail  string
		wantErr     error
		wantObserve bool
	}{
		{"legacy binding", "", nil, true}, {"begin failure", "begin", errStoreUnit, false}, {"lock failure", "lock-error", errStoreUnit, false}, {"confirmation failure", "outcome-error", errStoreUnit, false}, {"missing outcome ignored", "outcome-missing", nil, true}, {"conflict ignored", "request-missing", nil, true}, {"commit failure", "commit-error", errStoreUnit, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			rows := &reconciliationRows{items: []reconciliationEntry{{claim: "claim", host: "host", device: "device", request: "request", created: at, binding: true}}}
			query := func(context.Context, string, ...any) (pgx.Rows, error) { return rows, nil }
			tx := &confirmTestTx{t: t, mode: tc.fail, at: at, key: make([]byte, 32)}
			begin := func(context.Context) (pgx.Tx, error) {
				if tc.fail == "begin" {
					return nil, errStoreUnit
				}
				return tx, nil
			}
			observed := false
			observe := func(_ context.Context, claim, device string) (PairingOutcome, error) {
				if claim != "claim" || device != "device" {
					t.Fatal("wrong outcome scope")
				}
				observed = true
				return PairingOutcome{}, nil
			}
			err := reconcilePairingOutcomes(context.Background(), query, begin, observe, at, true)
			if !errors.Is(err, tc.wantErr) || observed != tc.wantObserve || !rows.closed {
				t.Fatalf("err=%v observed=%v steps=%v", err, observed, tx.steps)
			}
		})
	}
}
