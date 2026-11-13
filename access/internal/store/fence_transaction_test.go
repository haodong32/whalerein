package store

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgconn"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

var errFenceTest = errors.New("synthetic transaction failure")

type generationRow struct {
	values []int64
	err    error
}

func (r generationRow) Scan(dest ...any) error {
	if r.err != nil {
		return r.err
	}
	if len(dest) != len(r.values) {
		panic("invalid generation shape")
	}
	for i, value := range r.values {
		*dest[i].(*int64) = value
	}
	return nil
}

type commitFenceTx struct {
	pgx.Tx
	t                                          *testing.T
	fail                                       string
	device, host, capturedDevice, capturedHost int64
	steps                                      []string
}

func (tx *commitFenceTx) Exec(_ context.Context, sql string, args ...any) (pgconn.CommandTag, error) {
	tx.steps = append(tx.steps, "lock")
	if !strings.Contains(sql, "pg_advisory_xact_lock") || len(args) != 1 || args[0] != "device" {
		tx.t.Fatal("device lock must be first")
	}
	if tx.fail == "lock" {
		return pgconn.CommandTag{}, errFenceTest
	}
	return pgconn.NewCommandTag("SELECT 1"), nil
}
func (tx *commitFenceTx) QueryRow(_ context.Context, sql string, args ...any) pgx.Row {
	var step string
	var values []any
	switch {
	case strings.Contains(sql, "FROM devices"):
		step, values = "device", []any{tx.device}
		if len(args) != 1 || args[0] != "device" {
			tx.t.Fatal("wrong device scope")
		}
	case strings.Contains(sql, "FROM pairing_operations"):
		step, values = "captured", []any{tx.capturedDevice, tx.capturedHost, []byte(nil)}
		if len(args) != 3 || args[0] != "claim" || args[1] != "device" || args[2] != "host" {
			tx.t.Fatal("wrong claim scope")
		}
	case strings.Contains(sql, "FROM grants"):
		step, values = "host", []any{tx.host}
		if len(args) != 2 || args[0] != "host" || args[1] != "device" {
			tx.t.Fatal("wrong host scope")
		}
	default:
		tx.t.Fatal("unexpected fence query")
	}
	tx.steps = append(tx.steps, step)
	if tx.fail == step {
		return commitPairingRow{err: errFenceTest}
	}
	if tx.fail == "finished" && step == "captured" {
		values[2] = []byte{1}
	}
	return commitPairingRow{values: values}
}
func TestCommitPairingChecksBothFencesUnderTheDeviceLock(t *testing.T) {
	for _, tc := range []struct {
		name, fail   string
		device, host int64
		want         error
		steps        []string
	}{
		{"matching generations", "", 2, 3, nil, []string{"lock", "device", "captured", "host"}},
		{"device revoked", "", 4, 3, ErrPairingFenced, []string{"lock", "device", "captured", "host"}},
		{"host revoked", "", 2, 4, ErrPairingFenced, []string{"lock", "device", "captured", "host"}},
		{"already finished", "finished", 2, 3, nil, []string{"lock", "device", "captured"}},
		{"lock fails", "lock", 2, 3, errFenceTest, []string{"lock"}},
		{"device read fails", "device", 2, 3, errFenceTest, []string{"lock", "device"}},
		{"claim read fails", "captured", 2, 3, errFenceTest, []string{"lock", "device", "captured"}},
		{"host read fails", "host", 2, 3, errFenceTest, []string{"lock", "device", "captured", "host"}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			tx := &commitFenceTx{t: t, fail: tc.fail, device: tc.device, host: tc.host, capturedDevice: 2, capturedHost: 3}
			proceed, err := verifyPairingFences(context.Background(), tx, &PairingOperation{ClaimID: "claim"}, "host", "device")
			if proceed != (tc.name == "matching generations") || !errors.Is(err, tc.want) || !reflect.DeepEqual(tx.steps, tc.steps) {
				t.Fatalf("error=%v steps=%v", err, tx.steps)
			}
		})
	}
}

type revokeTestRow struct {
	value any
	err   error
}

func (r revokeTestRow) Scan(dest ...any) error {
	if r.err != nil {
		return r.err
	}
	switch v := r.value.(type) {
	case string:
		*dest[0].(*string) = v
	case bool:
		*dest[0].(*bool) = v
	default:
		panic("unexpected value")
	}
	return nil
}

type revokeGrantTx struct {
	pgx.Tx
	t        *testing.T
	fail     string
	affected int64
	exists   bool
	steps    []string
}

func (tx *revokeGrantTx) Exec(_ context.Context, sql string, args ...any) (pgconn.CommandTag, error) {
	step := "credentials"
	if strings.Contains(sql, "pg_advisory_xact_lock") {
		step = "lock"
	} else if strings.Contains(sql, "UPDATE grants") {
		step = "grant"
	}
	tx.steps = append(tx.steps, step)
	switch step {
	case "lock", "credentials":
		if len(args) != 1 || args[0] != "device" {
			tx.t.Fatal("wrong device scope")
		}
	case "grant":
		if len(args) != 3 || args[0] != "host" || args[1] != "device" {
			tx.t.Fatal("wrong grant scope")
		}
	}
	if tx.fail == step {
		return pgconn.CommandTag{}, errFenceTest
	}
	if step == "grant" {
		return pgconn.NewCommandTag(fmt.Sprintf("UPDATE %d", tx.affected)), nil
	}
	return pgconn.NewCommandTag("SELECT 1"), nil
}
func (tx *revokeGrantTx) QueryRow(_ context.Context, sql string, args ...any) pgx.Row {
	step := "device"
	var value any = "device"
	if strings.Contains(sql, "SELECT EXISTS") {
		step = "exists"
		value = tx.exists
		if len(args) != 2 || args[0] != "host" || args[1] != "device" {
			tx.t.Fatal("wrong existence scope")
		}
	} else if len(args) != 1 || args[0] != "device" {
		tx.t.Fatal("wrong device scope")
	}
	tx.steps = append(tx.steps, step)
	if tx.fail == step {
		return revokeTestRow{err: errFenceTest}
	}
	return revokeTestRow{value: value}
}
func (tx *revokeGrantTx) Commit(context.Context) error {
	tx.steps = append(tx.steps, "commit")
	if tx.fail == "commit" {
		return errFenceTest
	}
	return nil
}
func (tx *revokeGrantTx) Rollback(context.Context) error {
	tx.steps = append(tx.steps, "rollback")
	return nil
}
func TestRevokeGrantKeepsDeviceFenceThroughGrantAndCredentialChanges(t *testing.T) {
	for _, tc := range []struct {
		name, fail string
		affected   int64
		exists     bool
		want       error
		steps      []string
	}{
		{"revokes live grant", "", 1, true, nil, []string{"lock", "device", "grant", "credentials", "commit", "rollback"}},
		{"missing grant", "", 0, false, ErrNotFound, []string{"lock", "device", "grant", "exists", "rollback"}},
		{"existing no-op grant", "", 0, true, nil, []string{"lock", "device", "grant", "exists", "credentials", "commit", "rollback"}},
		{"lock failure", "lock", 1, true, errFenceTest, []string{"lock", "rollback"}},
		{"device failure", "device", 1, true, errFenceTest, []string{"lock", "device", "rollback"}},
		{"grant failure", "grant", 1, true, errFenceTest, []string{"lock", "device", "grant", "rollback"}},
		{"existence failure", "exists", 0, true, errFenceTest, []string{"lock", "device", "grant", "exists", "rollback"}},
		{"credential failure", "credentials", 1, true, errFenceTest, []string{"lock", "device", "grant", "credentials", "rollback"}},
		{"commit failure", "commit", 1, true, errFenceTest, []string{"lock", "device", "grant", "credentials", "commit", "rollback"}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			tx := &revokeGrantTx{t: t, fail: tc.fail, affected: tc.affected, exists: tc.exists}
			err := revokeGrant(context.Background(), func(context.Context) (pgx.Tx, error) { return tx, nil }, "host", "device", time.Time{})
			if !errors.Is(err, tc.want) || !reflect.DeepEqual(tx.steps, tc.steps) {
				t.Fatalf("error=%v steps=%v", err, tx.steps)
			}
		})
	}
	t.Run("begin failure", func(t *testing.T) {
		err := revokeGrant(context.Background(), func(context.Context) (pgx.Tx, error) { return nil, errFenceTest }, "host", "device", time.Time{})
		if !errors.Is(err, errFenceTest) {
			t.Fatal(err)
		}
	})
}

type revokeRows struct {
	pgx.Rows
	items []string
	index int
	fail  string
}

func (r *revokeRows) Next() bool { r.index++; return r.index <= len(r.items) }
func (r *revokeRows) Scan(dest ...any) error {
	if r.fail == "scan" {
		return errFenceTest
	}
	*dest[0].(*string) = r.items[r.index-1]
	return nil
}
func (r *revokeRows) Close() {}
func (r *revokeRows) Err() error {
	if r.fail == "rows" {
		return errFenceTest
	}
	return nil
}

type revokeDeviceTx struct {
	pgx.Tx
	t     *testing.T
	fail  string
	steps []string
}

func (tx *revokeDeviceTx) Exec(_ context.Context, sql string, args ...any) (pgconn.CommandTag, error) {
	step := "credentials"
	if strings.Contains(sql, "pg_advisory_xact_lock") {
		step = "lock"
	} else if strings.Contains(sql, "UPDATE devices") {
		step = "generation"
	}
	tx.steps = append(tx.steps, step)
	if len(args) != 1 || args[0] != "device" {
		tx.t.Fatal("wrong device scope")
	}
	if tx.fail == step {
		return pgconn.CommandTag{}, errFenceTest
	}
	return pgconn.NewCommandTag("UPDATE 1"), nil
}
func (tx *revokeDeviceTx) QueryRow(_ context.Context, _ string, args ...any) pgx.Row {
	tx.steps = append(tx.steps, "device")
	if len(args) != 1 || args[0] != "device" {
		tx.t.Fatal("wrong device scope")
	}
	if tx.fail == "missing" {
		return revokeTestRow{err: pgx.ErrNoRows}
	}
	if tx.fail == "device" {
		return revokeTestRow{err: errFenceTest}
	}
	return revokeTestRow{value: "device"}
}
func (tx *revokeDeviceTx) Query(_ context.Context, sql string, args ...any) (pgx.Rows, error) {
	tx.steps = append(tx.steps, "grants")
	if !strings.Contains(sql, "UPDATE grants") || len(args) != 2 || args[0] != "device" {
		tx.t.Fatal("wrong grant scope")
	}
	if tx.fail == "grants" {
		return nil, errFenceTest
	}
	return &revokeRows{items: []string{"host-a", "host-b"}, fail: tx.fail}, nil
}
func (tx *revokeDeviceTx) Commit(context.Context) error {
	tx.steps = append(tx.steps, "commit")
	if tx.fail == "commit" {
		return errFenceTest
	}
	return nil
}
func (tx *revokeDeviceTx) Rollback(context.Context) error {
	tx.steps = append(tx.steps, "rollback")
	return nil
}
func TestRevokeDeviceFencesAllGrantsAndCredentialsInOneTransaction(t *testing.T) {
	for _, tc := range []struct {
		name, fail string
		exists     bool
		want       error
		steps      []string
	}{
		{"success", "", true, nil, []string{"lock", "device", "generation", "grants", "credentials", "commit", "rollback"}},
		{"missing device", "missing", false, nil, []string{"lock", "device", "rollback"}},
		{"lock failure", "lock", false, errFenceTest, []string{"lock", "rollback"}},
		{"device read failure", "device", false, errFenceTest, []string{"lock", "device", "rollback"}},
		{"generation failure", "generation", true, errFenceTest, []string{"lock", "device", "generation", "rollback"}},
		{"grant failure", "grants", true, errFenceTest, []string{"lock", "device", "generation", "grants", "rollback"}},
		{"scan failure", "scan", true, errFenceTest, []string{"lock", "device", "generation", "grants", "rollback"}},
		{"rows failure", "rows", true, errFenceTest, []string{"lock", "device", "generation", "grants", "rollback"}},
		{"credential failure", "credentials", true, errFenceTest, []string{"lock", "device", "generation", "grants", "credentials", "rollback"}},
		{"commit failure", "commit", true, errFenceTest, []string{"lock", "device", "generation", "grants", "credentials", "commit", "rollback"}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			tx := &revokeDeviceTx{t: t, fail: tc.fail}
			hosts, exists, err := revokeDeviceGrants(context.Background(), func(context.Context) (pgx.Tx, error) { return tx, nil }, "device", time.Time{})
			if exists != tc.exists || !errors.Is(err, tc.want) || !reflect.DeepEqual(tx.steps, tc.steps) {
				t.Fatalf("exists=%v error=%v steps=%v", exists, err, tx.steps)
			}
			if tc.name == "success" && !reflect.DeepEqual(hosts, []string{"host-a", "host-b"}) {
				t.Fatalf("hosts=%v", hosts)
			}
		})
	}
	t.Run("begin failure", func(t *testing.T) {
		_, exists, err := revokeDeviceGrants(context.Background(), func(context.Context) (pgx.Tx, error) { return nil, errFenceTest }, "device", time.Time{})
		if exists || !errors.Is(err, errFenceTest) {
			t.Fatalf("exists=%v error=%v", exists, err)
		}
	})
}

type writePairingTx struct {
	pgx.Tx
	t     *testing.T
	fail  string
	zero  string
	steps []string
}

func (tx *writePairingTx) Exec(_ context.Context, sql string, args ...any) (pgconn.CommandTag, error) {
	step := "grant"
	switch {
	case strings.Contains(sql, "INSERT INTO devices"):
		step = "device"
	case strings.Contains(sql, "DELETE FROM device_credentials"):
		step = "delete-old"
	case strings.Contains(sql, "INSERT INTO device_credentials"):
		step = "credential"
	}
	tx.steps = append(tx.steps, step)
	switch step {
	case "device":
		if len(args) != 3 || args[0] != "device" || args[1] != "fingerprint" {
			tx.t.Fatal("wrong device identity")
		}
	case "delete-old":
		if len(args) != 2 || args[0] != "device" || args[1] != "host" {
			tx.t.Fatal("wrong credential scope")
		}
	case "grant":
		if len(args) != 3 || args[0] != "host" || args[1] != "device" {
			tx.t.Fatal("wrong grant scope")
		}
	}
	if tx.fail == step {
		return pgconn.CommandTag{}, errFenceTest
	}
	if tx.fail == "unique" && step == "device" {
		return pgconn.CommandTag{}, &pgconn.PgError{Code: "23505"}
	}
	if tx.zero == step {
		return pgconn.NewCommandTag("INSERT 0 0"), nil
	}
	return pgconn.NewCommandTag("INSERT 0 1"), nil
}
func TestWritePairedDeviceFailsClosedWithoutChangingOtherHosts(t *testing.T) {
	for _, tc := range []struct {
		name, fail, zero string
		want             error
		steps            []string
	}{
		{"success", "", "", nil, []string{"device", "delete-old", "credential", "grant"}},
		{"different fingerprint", "", "device", ErrConflict, []string{"device"}},
		{"device key collision", "unique", "", ErrConflict, []string{"device"}},
		{"device write failure", "device", "", errFenceTest, []string{"device"}},
		{"old credential cleanup failure", "delete-old", "", errFenceTest, []string{"device", "delete-old"}},
		{"credential belongs to another device", "", "credential", ErrConflict, []string{"device", "delete-old", "credential"}},
		{"credential write failure", "credential", "", errFenceTest, []string{"device", "delete-old", "credential"}},
		{"grant write failure", "grant", "", errFenceTest, []string{"device", "delete-old", "credential", "grant"}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			tx := &writePairingTx{t: t, fail: tc.fail, zero: tc.zero}
			err := writePairedDevice(context.Background(), tx, "host", "device", "fingerprint", []byte{1}, time.Time{})
			if !errors.Is(err, tc.want) || !reflect.DeepEqual(tx.steps, tc.steps) {
				t.Fatalf("error=%v steps=%v", err, tx.steps)
			}
		})
	}
}

type commitPairingRow struct {
	values []any
	err    error
}

func (r commitPairingRow) Scan(dest ...any) error {
	if r.err != nil {
		return r.err
	}
	if len(dest) != len(r.values) {
		panic("invalid scan shape")
	}
	for i, v := range r.values {
		switch x := v.(type) {
		case int64:
			*dest[i].(*int64) = x
		case uint64:
			*dest[i].(*uint64) = x
		case []byte:
			*dest[i].(*[]byte) = x
		case time.Time:
			*dest[i].(*time.Time) = x
		default:
			panic("invalid scan type")
		}
	}
	return nil
}

type commitPairingTx struct {
	pgx.Tx
	t                                         *testing.T
	fail                                      string
	steps                                     []string
	insertedNotice                            []byte
	device, host                              int64
	tokenUsed                                 bool
	databaseNow, tokenExpiry, operationExpiry time.Time
}

func (tx *commitPairingTx) Exec(_ context.Context, sql string, args ...any) (pgconn.CommandTag, error) {
	step := "unknown"
	switch {
	case strings.Contains(sql, "pg_advisory_xact_lock"):
		step = "lock"
	case strings.Contains(sql, "UPDATE pairing_tokens"):
		step = "token"
	case strings.Contains(sql, "INSERT INTO devices"):
		step = "device-write"
	case strings.Contains(sql, "DELETE FROM device_credentials"):
		step = "credential-cleanup"
	case strings.Contains(sql, "INSERT INTO device_credentials"):
		step = "credential-write"
	case strings.Contains(sql, "INSERT INTO grants"):
		step = "grant-write"
	case strings.Contains(sql, "INSERT INTO requests"):
		step = "notice-write"
		if len(args) > 5 {
			tx.insertedNotice = append([]byte(nil), args[5].([]byte)...)
		}
	case strings.Contains(sql, "UPDATE pairing_operations"):
		step = "operation-write"
	case strings.Contains(sql, "INSERT INTO pairing_outcomes"):
		step = "outcome-write"
	}
	if step == "unknown" {
		tx.t.Fatal("unexpected transaction statement")
	}
	tx.steps = append(tx.steps, step)
	if tx.fail == step {
		return pgconn.CommandTag{}, errFenceTest
	}
	if step == "token" && tx.tokenUsed {
		return pgconn.NewCommandTag("UPDATE 0"), nil
	}
	if step == "operation-write" && tx.fail == "operation-conflict" {
		return pgconn.NewCommandTag("UPDATE 0"), nil
	}
	return pgconn.NewCommandTag("UPDATE 1"), nil
}
func (tx *commitPairingTx) QueryRow(_ context.Context, sql string, _ ...any) pgx.Row {
	step := "grant-generation"
	values := []any{uint64(9)}
	switch {
	case strings.Contains(sql, "SELECT clock_timestamp()"):
		step = "deadline"
		at, tokenExpiry, operationExpiry := tx.databaseNow, tx.tokenExpiry, tx.operationExpiry
		if at.IsZero() {
			at = time.Now().UTC()
		}
		if tokenExpiry.IsZero() {
			tokenExpiry = at.Add(time.Minute)
		}
		if operationExpiry.IsZero() {
			operationExpiry = at.Add(time.Minute)
		}
		values = []any{at, tokenExpiry, operationExpiry}
	case strings.Contains(sql, "FROM devices"):
		step = "device-generation"
		values = []any{tx.device}
	case strings.Contains(sql, "FROM pairing_operations"):
		step = "captured"
		values = []any{int64(2), int64(3), []byte(nil)}
		if tx.fail == "finished" {
			values[2] = []byte{1}
		}
	case strings.Contains(sql, "revocation_generation FROM grants"):
		step = "host-generation"
		values = []any{tx.host}
	}
	tx.steps = append(tx.steps, step)
	if tx.fail == step {
		return commitPairingRow{err: errFenceTest}
	}
	return commitPairingRow{values: values}
}
func (tx *commitPairingTx) Commit(context.Context) error {
	tx.steps = append(tx.steps, "commit")
	if tx.fail == "commit" {
		return errFenceTest
	}
	return nil
}
func (tx *commitPairingTx) Rollback(context.Context) error {
	tx.steps = append(tx.steps, "rollback")
	return nil
}
func TestCommitPairingFencedTransactionPublishesOnlyAfterCommit(t *testing.T) {
	for _, tc := range []struct {
		name, fail   string
		device, host int64
		tokenUsed    bool
		want         error
		accepted     bool
	}{
		{"success", "", 2, 3, false, nil, true},
		{"device fence changed", "", 4, 3, false, ErrPairingFenced, false},
		{"host fence changed", "", 2, 4, false, ErrPairingFenced, false},
		{"token already used", "", 2, 3, true, nil, false},
		{"already finished", "finished", 2, 3, false, nil, false},
		{"lock failure", "lock", 2, 3, false, errFenceTest, false},
		{"token failure", "token", 2, 3, false, errFenceTest, false},
		{"device write failure", "device-write", 2, 3, false, errFenceTest, false},
		{"credential cleanup failure", "credential-cleanup", 2, 3, false, errFenceTest, false},
		{"credential write failure", "credential-write", 2, 3, false, errFenceTest, false},
		{"grant write failure", "grant-write", 2, 3, false, errFenceTest, false},
		{"grant generation read failure", "grant-generation", 2, 3, false, errFenceTest, false},
		{"notice write failure", "notice-write", 2, 3, false, errFenceTest, false},
		{"operation write failure", "operation-write", 2, 3, false, errFenceTest, false},
		{"outcome write failure", "outcome-write", 2, 3, false, errFenceTest, false},
		{"deadline read failure", "deadline", 2, 3, false, errFenceTest, false},
		{"operation conflict", "operation-conflict", 2, 3, false, ErrConflict, false},
		{"commit failure", "commit", 2, 3, false, errFenceTest, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			fixture := newBoundCommitFixture(t)
			response := fixture.response
			responseBody, _ := proto.Marshal(response)
			claimBody, _ := proto.Marshal(fixture.claim)
			notice := &whalereinv1.HostRequest{RequestId: "request", HostId: "host", DeviceId: "device", Kind: whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE, Body: append([]byte(nil), claimBody...), ProtocolVersion: 1, ExpiresAtUnixMs: fixture.now.Add(time.Minute).UnixMilli()}
			op := &PairingOperation{ClaimID: "claim", FinishHash: make([]byte, 32), FinishResponse: responseBody}
			tx := &commitPairingTx{t: t, fail: tc.fail, device: tc.device, host: tc.host, tokenUsed: tc.tokenUsed}
			accepted, err := commitPairing(context.Background(), func(context.Context) (pgx.Tx, error) { return tx, nil }, fixture.tokenHash, "host", "device", fixture.fingerprint, fixture.authHash, fixture.now, notice, op)
			if accepted != tc.accepted || !errors.Is(err, tc.want) {
				t.Fatalf("accepted=%v error=%v steps=%v", accepted, err, tx.steps)
			}
			if tx.steps[len(tx.steps)-1] != "rollback" {
				t.Fatalf("missing rollback defer: %v", tx.steps)
			}
			if !tc.accepted && !bytes.Equal(notice.Body, claimBody) {
				t.Fatal("failed transaction changed outward notice")
			}
			if errors.Is(tc.want, ErrPairingFenced) || tc.name == "already finished" {
				for _, step := range tx.steps {
					if step == "token" {
						t.Fatal("fenced transaction consumed token")
					}
				}
			}
			if tc.accepted {
				var decoded whalereinv1.PairingClaim
				if err := proto.Unmarshal(notice.Body, &decoded); err != nil || decoded.GrantGeneration != 9 || !bytes.Equal(notice.Body, tx.insertedNotice) {
					t.Fatal("notice generation did not match durable outbox")
				}
			}
		})
	}
}

type pairingFencedTx struct {
	pgx.Tx
	t                          *testing.T
	fail                       string
	finished                   bool
	currentDevice, currentHost int64
	steps                      []string
}

func (tx *pairingFencedTx) Exec(_ context.Context, sql string, args ...any) (pgconn.CommandTag, error) {
	tx.steps = append(tx.steps, "lock")
	if !strings.Contains(sql, "pg_advisory_xact_lock") || len(args) != 1 || args[0] != "device" {
		tx.t.Fatal("wrong device fence")
	}
	if tx.fail == "lock" {
		return pgconn.CommandTag{}, errFenceTest
	}
	return pgconn.NewCommandTag("SELECT 1"), nil
}
func (tx *pairingFencedTx) QueryRow(_ context.Context, sql string, args ...any) pgx.Row {
	step := "current"
	values := []any{tx.currentDevice, tx.currentHost}
	if strings.Contains(sql, "FROM pairing_operations") {
		step = "captured"
		values = []any{int64(2), int64(3), []byte(nil)}
		if tx.finished {
			values[2] = []byte{1}
		}
		if len(args) != 3 || args[0] != "claim" || args[1] != "host" || args[2] != "device" {
			tx.t.Fatal("wrong claim scope")
		}
	} else if len(args) != 2 || args[0] != "device" || args[1] != "host" {
		tx.t.Fatal("wrong generation scope")
	}
	tx.steps = append(tx.steps, step)
	if tx.fail == step {
		return commitPairingRow{err: errFenceTest}
	}
	return commitPairingRow{values: values}
}
func (tx *pairingFencedTx) Rollback(context.Context) error {
	tx.steps = append(tx.steps, "rollback")
	return nil
}
func TestPairingFencedNeverDiscardsFinishedClaimAndChecksBothGenerations(t *testing.T) {
	for _, tc := range []struct {
		name, fail   string
		finished     bool
		device, host int64
		want         bool
		wantErr      error
		steps        []string
	}{
		{"still valid", "", false, 2, 3, false, nil, []string{"lock", "captured", "current", "rollback"}},
		{"device revoked", "", false, 4, 3, true, nil, []string{"lock", "captured", "current", "rollback"}},
		{"host revoked", "", false, 2, 4, true, nil, []string{"lock", "captured", "current", "rollback"}},
		{"finished remains recoverable", "", true, 4, 4, false, nil, []string{"lock", "captured", "rollback"}},
		{"lock fails", "lock", false, 2, 3, false, errFenceTest, []string{"lock", "rollback"}},
		{"claim read fails", "captured", false, 2, 3, false, errFenceTest, []string{"lock", "captured", "rollback"}},
		{"current generation read fails", "current", false, 2, 3, false, errFenceTest, []string{"lock", "captured", "current", "rollback"}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			tx := &pairingFencedTx{t: t, fail: tc.fail, finished: tc.finished, currentDevice: tc.device, currentHost: tc.host}
			got, err := pairingFenced(context.Background(), func(context.Context) (pgx.Tx, error) { return tx, nil }, "claim", "host", "device")
			if got != tc.want || !errors.Is(err, tc.wantErr) || !reflect.DeepEqual(tx.steps, tc.steps) {
				t.Fatalf("fenced=%v error=%v steps=%v", got, err, tx.steps)
			}
		})
	}
	t.Run("begin failure", func(t *testing.T) {
		_, err := pairingFenced(context.Background(), func(context.Context) (pgx.Tx, error) { return nil, errFenceTest }, "claim", "host", "device")
		if !errors.Is(err, errFenceTest) {
			t.Fatal("begin error lost")
		}
	})

}

type saveFenceRow struct {
	value any
	err   error
}

func (r saveFenceRow) Scan(d ...any) error {
	if r.err != nil {
		return r.err
	}
	switch x := r.value.(type) {
	case int64:
		*d[0].(*int64) = x
	case bool:
		*d[0].(*bool) = x
	case []time.Time:
		*d[0].(*time.Time) = x[0]
		*d[1].(*time.Time) = x[1]
	default:
		panic("unexpected value")
	}
	return nil
}

type saveFenceTx struct {
	pgx.Tx
	t                        *testing.T
	device, host             int64
	completed                bool
	fail                     string
	steps                    []string
	databaseNow, tokenExpiry time.Time
}

func (tx *saveFenceTx) Exec(_ context.Context, sql string, args ...any) (pgconn.CommandTag, error) {
	step := "insert"
	if strings.Contains(sql, "pg_advisory_xact_lock") {
		step = "lock"
		if len(args) != 1 || args[0] != "device" {
			tx.t.Fatal("wrong lock scope")
		}
	}
	tx.steps = append(tx.steps, step)
	if tx.fail == step {
		return pgconn.CommandTag{}, errFenceTest
	}
	if tx.fail == "unique" && step == "insert" {
		return pgconn.CommandTag{}, &pgconn.PgError{Code: "23505"}
	}
	return pgconn.NewCommandTag("INSERT 0 1"), nil
}
func (tx *saveFenceTx) QueryRow(_ context.Context, sql string, args ...any) pgx.Row {
	step := "device"
	var value any = tx.device
	if strings.Contains(sql, "SELECT clock_timestamp()") {
		step = "deadline"
		at, tokenExpiry := tx.databaseNow, tx.tokenExpiry
		if at.IsZero() {
			at = time.Now().UTC()
		}
		if tokenExpiry.IsZero() {
			tokenExpiry = at.Add(time.Minute)
		}
		value = []time.Time{at, tokenExpiry}
		if len(args) != 2 || args[1] != "host" {
			tx.t.Fatal("wrong token deadline scope")
		}
	} else if strings.Contains(sql, "FROM grants") {
		step = "host"
		value = tx.host
		if len(args) != 2 || args[0] != "host" || args[1] != "device" {
			tx.t.Fatal("wrong host scope")
		}
	} else if strings.Contains(sql, "pairing_outcomes") {
		step = "outcome"
		value = tx.completed
		if len(args) != 1 || args[0] != "claim" {
			tx.t.Fatal("wrong claim scope")
		}
	} else if len(args) != 1 || args[0] != "device" {
		tx.t.Fatal("wrong device scope")
	}
	tx.steps = append(tx.steps, step)
	if tx.fail == step {
		return saveFenceRow{err: errFenceTest}
	}
	return saveFenceRow{value: value}
}
func (tx *saveFenceTx) Commit(context.Context) error {
	tx.steps = append(tx.steps, "commit")
	if tx.fail == "commit" {
		return errFenceTest
	}
	return nil
}
func (tx *saveFenceTx) Rollback(context.Context) error {
	tx.steps = append(tx.steps, "rollback")
	return nil
}
func TestSavePairingOperationRefusesCompletedOutcomeAndChangedFences(t *testing.T) {
	for _, tc := range []struct {
		name, fail   string
		device, host int64
		completed    bool
		want         error
	}{
		{"first claim", "", 2, 3, false, nil}, {"completed tombstone", "", 2, 3, true, ErrConflict}, {"device revoked", "", 4, 3, false, ErrPairingFenced}, {"host revoked", "", 2, 4, false, ErrPairingFenced},
		{"lock failure", "lock", 2, 3, false, errFenceTest}, {"device read failure", "device", 2, 3, false, errFenceTest}, {"host read failure", "host", 2, 3, false, errFenceTest}, {"outcome read failure", "outcome", 2, 3, false, errFenceTest}, {"insert failure", "insert", 2, 3, false, errFenceTest}, {"claim collision", "unique", 2, 3, false, ErrConflict}, {"deadline read failure", "deadline", 2, 3, false, errFenceTest}, {"commit failure", "commit", 2, 3, false, errFenceTest},
	} {
		t.Run(tc.name, func(t *testing.T) {
			tx := &saveFenceTx{t: t, fail: tc.fail, device: tc.device, host: tc.host, completed: tc.completed}
			err := savePairingOperation(context.Background(), func(context.Context) (pgx.Tx, error) { return tx, nil }, PairingOperation{ClaimID: "claim", HostID: "host", DeviceID: "device", DeviceGeneration: 2, HostGeneration: 3, ExpiresAt: time.Now().UTC().Add(time.Minute)})
			if !errors.Is(err, tc.want) {
				t.Fatalf("unexpected error: %v", err)
			}
			if len(tx.steps) == 0 || tx.steps[len(tx.steps)-1] != "rollback" {
				t.Fatal("transaction not closed")
			}
			if tc.completed {
				for _, step := range tx.steps {
					if step == "insert" || step == "commit" {
						t.Fatal("completed claim was reopened")
					}
				}
			}
		})
	}
}
