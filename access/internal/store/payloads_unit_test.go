package store

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgconn"
)

// payloadScanRow models one temporary_ciphertext_objects row.
type payloadScanRow struct {
	object PayloadObject
	err    error
}

func (r payloadScanRow) Scan(dest ...any) error {
	if r.err != nil {
		return r.err
	}
	object := r.object
	*dest[0].(*string) = object.ObjectID
	*dest[1].(*string) = object.NodeID
	*dest[2].(*string) = object.OwnerID
	*dest[3].(*string) = object.HostID
	*dest[4].(*string) = object.DeviceID
	*dest[5].(*string) = object.AppSession
	*dest[6].(*string) = object.ChannelID
	*dest[7].(*string) = object.RequestID
	*dest[8].(*string) = object.MessageID
	*dest[9].(*string) = object.EventID
	*dest[10].(*string) = object.Purpose
	*dest[11].(*string) = object.PartID
	*dest[12].(*string) = object.State
	*dest[13].(*int64) = object.Reserved
	*dest[14].(*int64) = object.Length
	*dest[15].(*[]byte) = object.Digest
	*dest[16].(*time.Time) = object.ExpiresAt
	*dest[17].(*time.Time) = object.CreatedAt
	*dest[18].(*time.Time) = object.UpdatedAt
	*dest[19].(**time.Time) = object.ReadyAt
	*dest[20].(*int) = object.ReaderCount
	*dest[21].(*int) = object.CleanupTries
	*dest[22].(**time.Time) = object.NextAttempt
	*dest[23].(**int64) = object.GrantGeneration
	*dest[24].(**int64) = object.RevocationGeneration
	*dest[25].(**int64) = object.DeviceRevocationGeneration
	return nil
}

// int64Row scans a fixed list of int64 values.
type int64Row struct {
	values []int64
	err    error
}

func (r int64Row) Scan(dest ...any) error {
	if r.err != nil {
		return r.err
	}
	if len(dest) != len(r.values) {
		return errors.New("column count mismatch")
	}
	for i, target := range dest {
		switch typed := target.(type) {
		case *int64:
			*typed = r.values[i]
		case *int:
			*typed = int(r.values[i])
		default:
			return errors.New("unsupported scan target")
		}
	}
	return nil
}

func payloadFixture() PayloadObject {
	now := time.Unix(1_700_000_000, 0).UTC()
	digest := make([]byte, 32)
	digest[0] = 1
	ready := now.Add(time.Hour)
	generation := int64(3)
	return PayloadObject{
		ObjectID: "0123456789abcdef0123456789abcdef", NodeID: "node-1", OwnerID: "node-1",
		HostID: "host-1", DeviceID: "device-1", RequestID: "request-1",
		Purpose: "result", State: PayloadReady, Reserved: 8, Length: 8, Digest: digest,
		ExpiresAt: now.Add(time.Hour), CreatedAt: now, UpdatedAt: now, ReadyAt: &ready,
		ReaderCount: 1, CleanupTries: 2, GrantGeneration: &generation,
		RevocationGeneration: &generation, DeviceRevocationGeneration: &generation,
	}
}

func payloadReservationFixture() PayloadReservation {
	digest := make([]byte, 32)
	return PayloadReservation{
		ObjectID: "0123456789abcdef0123456789abcdef", NodeID: "node-1", OwnerID: "node-1",
		HostID: "host-1", RequestID: "request-1", Purpose: "result",
		Length: 8, ReservedBytes: 8, Digest: digest, ExpiresAt: time.Unix(1_700_003_600, 0).UTC(),
	}
}

func payloadLimitsFixture() PayloadLimits {
	return PayloadLimits{MaxObjectBytes: 1 << 20, MaxHostBytes: 4 << 20, MaxNodeBytes: 8 << 20, MaxObjects: 10, MaxActiveUploads: 4}
}

func payloadTx(t *testing.T, totals int64Row, inserted PayloadObject) *scriptedTx {
	t.Helper()
	tx := &scriptedTx{}
	tx.execFn = func(context.Context, string, ...any) (pgconn.CommandTag, error) {
		return pgconn.NewCommandTag("INSERT 0 1"), nil
	}
	tx.queryRowFn = func(_ context.Context, sql string, _ ...any) pgx.Row {
		switch {
		case strings.Contains(sql, "COALESCE(SUM(reserved_bytes), 0)"):
			return totals
		case strings.Contains(sql, "FROM temporary_ciphertext_objects WHERE object_id"):
			return payloadScanRow{object: inserted}
		default:
			t.Fatalf("unexpected query row: %s", sql)
			return payloadScanRow{err: pgx.ErrNoRows}
		}
	}
	return tx
}

func TestReservePayloadValidatesBeforeDatabase(t *testing.T) {
	ctx := context.Background()
	valid := payloadReservationFixture()
	limits := payloadLimitsFixture()
	mutations := []struct {
		name   string
		mutate func(*PayloadReservation)
	}{
		{"missing id", func(r *PayloadReservation) { r.ObjectID = "" }},
		{"missing node", func(r *PayloadReservation) { r.NodeID = "" }},
		{"missing host", func(r *PayloadReservation) { r.HostID = "" }},
		{"missing purpose", func(r *PayloadReservation) { r.Purpose = "" }},
		{"zero length", func(r *PayloadReservation) { r.Length = 0 }},
		{"reserved below length", func(r *PayloadReservation) { r.ReservedBytes = 1 }},
		{"short digest", func(r *PayloadReservation) { r.Digest = []byte{1} }},
		{"missing expiry", func(r *PayloadReservation) { r.ExpiresAt = time.Time{} }},
	}
	for _, tc := range mutations {
		t.Run(tc.name, func(t *testing.T) {
			reservation := valid
			tc.mutate(&reservation)
			if _, err := newStore(&fakeDB{t: t}).ReservePayload(ctx, reservation, limits); err == nil {
				t.Fatal("invalid reservation was accepted")
			}
		})
	}
	if _, err := newStore(&fakeDB{t: t}).ReservePayload(ctx, valid, PayloadLimits{}); err == nil {
		t.Fatal("zero limits were accepted")
	}
}

func TestReservePayloadRefusesAtEveryBound(t *testing.T) {
	ctx := context.Background()
	cases := []struct {
		name    string
		totals  int64Row
		charges int64
		want    string
	}{
		{"object size", int64Row{values: []int64{0, 0, 0, 0}}, 2 << 20, PayloadRefusedSize},
		{"host quota", int64Row{values: []int64{0, 0, 4 << 20, 0}}, 8, PayloadRefusedQuota},
		{"node bytes", int64Row{values: []int64{8 << 20, 1, 0, 0}}, 8, PayloadRefusedCapacity},
		{"object count", int64Row{values: []int64{0, 10, 0, 0}}, 8, PayloadRefusedCapacity},
		{"active uploads", int64Row{values: []int64{0, 0, 0, 4}}, 8, PayloadRefusedCapacity},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			tx := payloadTx(t, tc.totals, PayloadObject{})
			db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
			reservation := payloadReservationFixture()
			reservation.ReservedBytes = tc.charges
			reservation.Length = tc.charges
			admission, err := newStore(db).ReservePayload(ctx, reservation, payloadLimitsFixture())
			if err != nil {
				t.Fatalf("ReservePayload: %v", err)
			}
			if admission.Reason != tc.want {
				t.Fatalf("reason = %q, want %q", admission.Reason, tc.want)
			}
		})
	}
}

func TestReservePayloadAdmitsAndReportsStorageFailures(t *testing.T) {
	ctx := context.Background()
	t.Run("success", func(t *testing.T) {
		inserted := payloadFixture()
		tx := payloadTx(t, int64Row{values: []int64{0, 0, 0, 0}}, inserted)
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		admission, err := newStore(db).ReservePayload(ctx, payloadReservationFixture(), payloadLimitsFixture())
		if err != nil || admission.Object.ObjectID != inserted.ObjectID || !tx.committed {
			t.Fatalf("admission = %+v err=%v committed=%v", admission, err, tx.committed)
		}
	})
	t.Run("duplicate", func(t *testing.T) {
		tx := payloadTx(t, int64Row{values: []int64{0, 0, 0, 0}}, PayloadObject{})
		tx.execFn = func(_ context.Context, sql string, _ ...any) (pgconn.CommandTag, error) {
			if strings.Contains(sql, "INSERT INTO") {
				return pgconn.CommandTag{}, &pgconn.PgError{Code: "23505"}
			}
			return pgconn.NewCommandTag("SELECT 1"), nil
		}
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		if _, err := newStore(db).ReservePayload(ctx, payloadReservationFixture(), payloadLimitsFixture()); !errors.Is(err, ErrPayloadDuplicate) {
			t.Fatalf("duplicate error = %v", err)
		}
	})
	t.Run("begin", func(t *testing.T) {
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return nil, errors.New("begin") }}
		if _, err := newStore(db).ReservePayload(ctx, payloadReservationFixture(), payloadLimitsFixture()); err == nil {
			t.Fatal("begin failure was ignored")
		}
	})
	t.Run("lock", func(t *testing.T) {
		tx := payloadTx(t, int64Row{values: []int64{0, 0, 0, 0}}, PayloadObject{})
		tx.execFn = func(context.Context, string, ...any) (pgconn.CommandTag, error) {
			return pgconn.CommandTag{}, errors.New("lock")
		}
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		if _, err := newStore(db).ReservePayload(ctx, payloadReservationFixture(), payloadLimitsFixture()); err == nil {
			t.Fatal("lock failure was ignored")
		}
	})
	t.Run("totals", func(t *testing.T) {
		tx := payloadTx(t, int64Row{err: errors.New("totals")}, PayloadObject{})
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		if _, err := newStore(db).ReservePayload(ctx, payloadReservationFixture(), payloadLimitsFixture()); err == nil {
			t.Fatal("totals failure was ignored")
		}
	})
	t.Run("insert", func(t *testing.T) {
		tx := payloadTx(t, int64Row{values: []int64{0, 0, 0, 0}}, PayloadObject{})
		tx.execFn = func(_ context.Context, sql string, _ ...any) (pgconn.CommandTag, error) {
			if strings.Contains(sql, "INSERT INTO") {
				return pgconn.CommandTag{}, errors.New("insert")
			}
			return pgconn.NewCommandTag("SELECT 1"), nil
		}
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		if _, err := newStore(db).ReservePayload(ctx, payloadReservationFixture(), payloadLimitsFixture()); err == nil {
			t.Fatal("insert failure was ignored")
		}
	})
	t.Run("read back", func(t *testing.T) {
		tx := payloadTx(t, int64Row{values: []int64{0, 0, 0, 0}}, PayloadObject{})
		tx.queryRowFn = func(_ context.Context, sql string, _ ...any) pgx.Row {
			if strings.Contains(sql, "COALESCE(SUM") {
				return int64Row{values: []int64{0, 0, 0, 0}}
			}
			return payloadScanRow{err: errors.New("read back")}
		}
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		if _, err := newStore(db).ReservePayload(ctx, payloadReservationFixture(), payloadLimitsFixture()); err == nil {
			t.Fatal("read-back failure was ignored")
		}
	})
	t.Run("commit", func(t *testing.T) {
		tx := payloadTx(t, int64Row{values: []int64{0, 0, 0, 0}}, payloadFixture())
		tx.commitErr = errors.New("commit")
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		if _, err := newStore(db).ReservePayload(ctx, payloadReservationFixture(), payloadLimitsFixture()); err == nil {
			t.Fatal("commit failure was ignored")
		}
	})
}

func TestPayloadLockKeysAreDeterministicAndOrdered(t *testing.T) {
	first := PayloadLockKey("node", "node-1")
	if first != PayloadLockKey("node", "node-1") {
		t.Fatal("lock key is not deterministic")
	}
	if first == PayloadLockKey("node", "node-2") {
		t.Fatal("different node ids share a lock key")
	}
	keys := orderedPayloadLocks("node-1", "host-1")
	if len(keys) != 2 || keys[0] > keys[1] {
		t.Fatalf("locks = %v", keys)
	}
}

func TestGetPayloadReportsOutcomes(t *testing.T) {
	expected := payloadFixture()
	db := &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row {
		return payloadScanRow{object: expected}
	}}
	object, err := newStore(db).GetPayload(context.Background(), expected.ObjectID)
	if err != nil || object.ObjectID != expected.ObjectID {
		t.Fatalf("object = %+v err=%v", object, err)
	}
	db = &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row {
		return payloadScanRow{err: pgx.ErrNoRows}
	}}
	if _, err := newStore(db).GetPayload(context.Background(), "missing"); !errors.Is(err, ErrNotFound) {
		t.Fatalf("missing = %v", err)
	}
	db = &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row {
		return payloadScanRow{err: errors.New("db")}
	}}
	if _, err := newStore(db).GetPayload(context.Background(), expected.ObjectID); err == nil {
		t.Fatal("database failure was ignored")
	}
}

func TestConfirmPayloadReadyChecksInputsAndOutcomes(t *testing.T) {
	ctx := context.Background()
	now := time.Unix(1_700_000_000, 0).UTC()
	if _, err := newStore(&fakeDB{t: t}).ConfirmPayloadReady(ctx, "id", 0, make([]byte, 32), now); err == nil {
		t.Fatal("zero length was accepted")
	}
	if _, err := newStore(&fakeDB{t: t}).ConfirmPayloadReady(ctx, "id", 1, []byte{1}, now); err == nil {
		t.Fatal("short digest was accepted")
	}
	expected := payloadFixture()
	db := &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row {
		return payloadScanRow{object: expected}
	}}
	if _, err := newStore(db).ConfirmPayloadReady(ctx, expected.ObjectID, 8, expected.Digest, now); err != nil {
		t.Fatalf("confirm: %v", err)
	}
	db = &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row {
		return payloadScanRow{err: pgx.ErrNoRows}
	}}
	if _, err := newStore(db).ConfirmPayloadReady(ctx, "missing", 8, expected.Digest, now); !errors.Is(err, ErrNotFound) {
		t.Fatalf("missing confirm = %v", err)
	}
}

func TestOpenPayloadReadChecksStateScopeAndExpiry(t *testing.T) {
	ctx := context.Background()
	now := time.Unix(1_700_000_000, 0).UTC()
	ready := payloadFixture()
	ready.ExpiresAt = now.Add(time.Hour)
	newTx := func(object PayloadObject, writeErr error) *scriptedTx {
		tx := &scriptedTx{}
		tx.queryRowFn = func(context.Context, string, ...any) pgx.Row { return payloadScanRow{object: object} }
		tx.execFn = func(context.Context, string, ...any) (pgconn.CommandTag, error) {
			if writeErr != nil {
				return pgconn.CommandTag{}, writeErr
			}
			return pgconn.NewCommandTag("UPDATE 1"), nil
		}
		return tx
	}
	t.Run("success", func(t *testing.T) {
		tx := newTx(ready, nil)
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		object, err := newStore(db).OpenPayloadRead(ctx, PayloadReadScope{ObjectID: ready.ObjectID}, "fedcba9876543210fedcba9876543210", now)
		if err != nil || object.ReaderCount != ready.ReaderCount+1 || !tx.committed {
			t.Fatalf("open = %+v err=%v committed=%v", object, err, tx.committed)
		}
	})
	t.Run("scope", func(t *testing.T) {
		tx := newTx(ready, nil)
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		_, err := newStore(db).OpenPayloadRead(ctx, PayloadReadScope{ObjectID: ready.ObjectID, HostID: "other"}, "fedcba9876543210fedcba9876543210", now)
		if !errors.Is(err, ErrPayloadScopeMismatch) {
			t.Fatalf("scope = %v", err)
		}
	})
	t.Run("not ready", func(t *testing.T) {
		tx := newTx(PayloadObject{ObjectID: ready.ObjectID, State: PayloadReserved}, nil)
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		_, err := newStore(db).OpenPayloadRead(ctx, PayloadReadScope{ObjectID: ready.ObjectID}, "fedcba9876543210fedcba9876543210", now)
		if !errors.Is(err, ErrNotFound) {
			t.Fatalf("not ready = %v", err)
		}
	})
	t.Run("expired", func(t *testing.T) {
		expired := ready
		expired.ExpiresAt = now
		tx := newTx(expired, nil)
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		_, err := newStore(db).OpenPayloadRead(ctx, PayloadReadScope{ObjectID: ready.ObjectID}, "fedcba9876543210fedcba9876543210", now)
		if !errors.Is(err, ErrNotFound) {
			t.Fatalf("expired = %v", err)
		}
	})
	t.Run("row and write failures", func(t *testing.T) {
		tx := &scriptedTx{}
		tx.queryRowFn = func(context.Context, string, ...any) pgx.Row { return payloadScanRow{err: errors.New("read")} }
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		if _, err := newStore(db).OpenPayloadRead(ctx, PayloadReadScope{ObjectID: ready.ObjectID}, "fedcba9876543210fedcba9876543210", now); err == nil {
			t.Fatal("row failure was ignored")
		}
		tx = newTx(ready, errors.New("lease"))
		db = &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		if _, err := newStore(db).OpenPayloadRead(ctx, PayloadReadScope{ObjectID: ready.ObjectID}, "fedcba9876543210fedcba9876543210", now); err == nil {
			t.Fatal("lease failure was ignored")
		}
	})
	t.Run("begin and commit", func(t *testing.T) {
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return nil, errors.New("begin") }}
		if _, err := newStore(db).OpenPayloadRead(ctx, PayloadReadScope{ObjectID: ready.ObjectID}, "fedcba9876543210fedcba9876543210", now); err == nil {
			t.Fatal("begin failure was ignored")
		}
		tx := newTx(ready, nil)
		tx.commitErr = errors.New("commit")
		db = &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		if _, err := newStore(db).OpenPayloadRead(ctx, PayloadReadScope{ObjectID: ready.ObjectID}, "fedcba9876543210fedcba9876543210", now); err == nil {
			t.Fatal("commit failure was ignored")
		}
	})
}

func TestReleasePayloadReadIsSingleStatement(t *testing.T) {
	ctx := context.Background()
	db := &fakeDB{t: t, execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
		return pgconn.NewCommandTag("SELECT 1"), nil
	}}
	if err := newStore(db).ReleasePayloadRead(ctx, "object", "lease"); err != nil {
		t.Fatalf("release: %v", err)
	}
	db = &fakeDB{t: t, execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
		return pgconn.CommandTag{}, errors.New("release")
	}}
	if err := newStore(db).ReleasePayloadRead(ctx, "object", "lease"); err == nil {
		t.Fatal("release failure was ignored")
	}
	if err := (&Store{}).ReleasePayloadRead(ctx, "object", "lease"); err == nil {
		t.Fatal("release without a pool must fail")
	}
}

func TestCancelPayloadClosesReservedAndReadyOnce(t *testing.T) {
	ctx := context.Background()
	now := time.Unix(1_700_000_000, 0).UTC()
	ready := payloadFixture()
	t.Run("reserved", func(t *testing.T) {
		tx := &scriptedTx{}
		tx.queryRowFn = func(_ context.Context, sql string, _ ...any) pgx.Row {
			if strings.Contains(sql, "UPDATE temporary_ciphertext_objects") {
				updated := ready
				updated.State = PayloadExpired
				return payloadScanRow{object: updated}
			}
			return payloadScanRow{object: PayloadObject{ObjectID: ready.ObjectID, State: PayloadReserved}}
		}
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		object, err := newStore(db).CancelPayload(ctx, PayloadReadScope{ObjectID: ready.ObjectID}, now)
		if err != nil || object.State != PayloadExpired || !tx.committed {
			t.Fatalf("cancel = %+v err=%v committed=%v", object, err, tx.committed)
		}
	})
	t.Run("already expired", func(t *testing.T) {
		tx := &scriptedTx{}
		tx.queryRowFn = func(context.Context, string, ...any) pgx.Row {
			return payloadScanRow{object: PayloadObject{ObjectID: ready.ObjectID, State: PayloadExpired}}
		}
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		object, err := newStore(db).CancelPayload(ctx, PayloadReadScope{ObjectID: ready.ObjectID}, now)
		if err != nil || object.State != PayloadExpired {
			t.Fatalf("cancel expired = %+v err=%v", object, err)
		}
	})
	t.Run("scope", func(t *testing.T) {
		tx := &scriptedTx{}
		tx.queryRowFn = func(context.Context, string, ...any) pgx.Row {
			return payloadScanRow{object: PayloadObject{ObjectID: ready.ObjectID, HostID: "host-1"}}
		}
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		_, err := newStore(db).CancelPayload(ctx, PayloadReadScope{ObjectID: ready.ObjectID, HostID: "other"}, now)
		if !errors.Is(err, ErrPayloadScopeMismatch) {
			t.Fatalf("scope = %v", err)
		}
	})
	t.Run("failures", func(t *testing.T) {
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return nil, errors.New("begin") }}
		if _, err := newStore(db).CancelPayload(ctx, PayloadReadScope{ObjectID: ready.ObjectID}, now); err == nil {
			t.Fatal("begin failure was ignored")
		}
		tx := &scriptedTx{}
		tx.queryRowFn = func(context.Context, string, ...any) pgx.Row { return payloadScanRow{err: errors.New("read")} }
		db = &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		if _, err := newStore(db).CancelPayload(ctx, PayloadReadScope{ObjectID: ready.ObjectID}, now); err == nil {
			t.Fatal("read failure was ignored")
		}
		tx = &scriptedTx{}
		tx.queryRowFn = func(_ context.Context, sql string, _ ...any) pgx.Row {
			if strings.Contains(sql, "UPDATE temporary_ciphertext_objects") {
				return payloadScanRow{err: errors.New("update")}
			}
			return payloadScanRow{object: PayloadObject{ObjectID: ready.ObjectID, State: PayloadReserved}}
		}
		db = &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		if _, err := newStore(db).CancelPayload(ctx, PayloadReadScope{ObjectID: ready.ObjectID}, now); err == nil {
			t.Fatal("update failure was ignored")
		}
	})
}

func TestExpirePayloadsBoundsAndScans(t *testing.T) {
	ctx := context.Background()
	now := time.Unix(1_700_000_000, 0).UTC()
	if objects, err := newStore(&fakeDB{t: t}).ExpirePayloads(ctx, now, 0); err != nil || objects != nil {
		t.Fatalf("zero limit = %v %v", objects, err)
	}
	db := &fakeDB{t: t, queryFn: func(context.Context, string, ...any) (pgx.Rows, error) {
		return &fakeRowSet{rows: []pgx.Row{payloadScanRow{object: payloadFixture()}}}, nil
	}}
	objects, err := newStore(db).ExpirePayloads(ctx, now, 5)
	if err != nil || len(objects) != 1 {
		t.Fatalf("expire = %+v err=%v", objects, err)
	}
	db = &fakeDB{t: t, queryFn: func(context.Context, string, ...any) (pgx.Rows, error) {
		return nil, errors.New("query")
	}}
	if _, err := newStore(db).ExpirePayloads(ctx, now, 5); err == nil {
		t.Fatal("query failure was ignored")
	}
	db = &fakeDB{t: t, queryFn: func(context.Context, string, ...any) (pgx.Rows, error) {
		return &fakeRowSet{err: errors.New("rows")}, nil
	}}
	if _, err := newStore(db).ExpirePayloads(ctx, now, 5); err == nil {
		t.Fatal("rows failure was ignored")
	}
	db = &fakeDB{t: t, queryFn: func(context.Context, string, ...any) (pgx.Rows, error) {
		return &fakeRowSet{rows: []pgx.Row{payloadScanRow{err: errors.New("scan")}}}, nil
	}}
	if _, err := newStore(db).ExpirePayloads(ctx, now, 5); err == nil {
		t.Fatal("scan failure was ignored")
	}
}

func TestClaimPayloadCleanupBoundsAndLease(t *testing.T) {
	ctx := context.Background()
	now := time.Unix(1_700_000_000, 0).UTC()
	if objects, err := newStore(&fakeDB{t: t}).ClaimPayloadCleanup(ctx, now, 0, 0); err != nil || objects != nil {
		t.Fatalf("zero limit = %v %v", objects, err)
	}
	seen := ""
	db := &fakeDB{t: t, queryFn: func(_ context.Context, _ string, args ...any) (pgx.Rows, error) {
		seen = args[2].(time.Time).Sub(now).String()
		return &fakeRowSet{rows: []pgx.Row{payloadScanRow{object: payloadFixture()}}}, nil
	}}
	objects, err := newStore(db).ClaimPayloadCleanup(ctx, now, 5, 0)
	if err != nil || len(objects) != 1 || seen != "1m0s" {
		t.Fatalf("claim = %+v err=%v lease=%s", objects, err, seen)
	}
	db = &fakeDB{t: t, queryFn: func(context.Context, string, ...any) (pgx.Rows, error) {
		return nil, errors.New("query")
	}}
	if _, err := newStore(db).ClaimPayloadCleanup(ctx, now, 5, time.Second); err == nil {
		t.Fatal("query failure was ignored")
	}
}

func TestPayloadMetadataTransitionsReportFailures(t *testing.T) {
	ctx := context.Background()
	now := time.Unix(1_700_000_000, 0).UTC()
	if err := newStore(&fakeDB{t: t, execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
		return pgconn.NewCommandTag("UPDATE 1"), nil
	}}).MarkPayloadCleanupFailure(ctx, "id", now, 0); err != nil {
		t.Fatalf("cleanup failure: %v", err)
	}
	if err := newStore(&fakeDB{t: t, execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
		return pgconn.CommandTag{}, errors.New("update")
	}}).MarkPayloadCleanupFailure(ctx, "id", now, time.Minute); err == nil {
		t.Fatal("cleanup failure write was ignored")
	}
	deleted, err := newStore(&fakeDB{t: t, execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
		return updateTag(1), nil
	}}).MarkPayloadDeleted(ctx, "id", now)
	if err != nil || !deleted {
		t.Fatalf("deleted = %v err=%v", deleted, err)
	}
	deleted, err = newStore(&fakeDB{t: t, execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
		return updateTag(0), nil
	}}).MarkPayloadDeleted(ctx, "id", now)
	if err != nil || deleted {
		t.Fatalf("already deleted = %v err=%v", deleted, err)
	}
	if _, err := newStore(&fakeDB{t: t, execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
		return pgconn.CommandTag{}, errors.New("delete")
	}}).MarkPayloadDeleted(ctx, "id", now); err == nil {
		t.Fatal("delete failure was ignored")
	}
}

func TestPayloadTombstonesStaleAndReadyPages(t *testing.T) {
	ctx := context.Background()
	now := time.Unix(1_700_000_000, 0).UTC()
	store := newStore(&fakeDB{t: t})
	if count, err := store.DeletePayloadTombstones(ctx, now, 0); err != nil || count != 0 {
		t.Fatalf("zero tombstones = %d %v", count, err)
	}
	if objects, err := store.StaleReservedPayloads(ctx, now, 0); err != nil || objects != nil {
		t.Fatalf("zero stale = %v %v", objects, err)
	}
	if objects, err := store.ReadyPayloadPage(ctx, "", 0); err != nil || objects != nil {
		t.Fatalf("zero ready = %v %v", objects, err)
	}
	db := &fakeDB{t: t, execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
		return deleteTag(3), nil
	}}
	if count, err := newStore(db).DeletePayloadTombstones(ctx, now, 5); err != nil || count != 3 {
		t.Fatalf("tombstones = %d %v", count, err)
	}
	db = &fakeDB{t: t, execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
		return pgconn.CommandTag{}, errors.New("delete")
	}}
	if _, err := newStore(db).DeletePayloadTombstones(ctx, now, 5); err == nil {
		t.Fatal("tombstone failure was ignored")
	}
	for name, call := range map[string]func(*Store) ([]PayloadObject, error){
		"stale": func(s *Store) ([]PayloadObject, error) { return s.StaleReservedPayloads(ctx, now, 5) },
		"ready": func(s *Store) ([]PayloadObject, error) { return s.ReadyPayloadPage(ctx, "", 5) },
	} {
		t.Run(name, func(t *testing.T) {
			db := &fakeDB{t: t, queryFn: func(context.Context, string, ...any) (pgx.Rows, error) {
				return &fakeRowSet{rows: []pgx.Row{payloadScanRow{object: payloadFixture()}}}, nil
			}}
			if objects, err := call(newStore(db)); err != nil || len(objects) != 1 {
				t.Fatalf("objects = %+v err=%v", objects, err)
			}
			db = &fakeDB{t: t, queryFn: func(context.Context, string, ...any) (pgx.Rows, error) {
				return nil, errors.New("query")
			}}
			if _, err := call(newStore(db)); err == nil {
				t.Fatal("query failure was ignored")
			}
		})
	}
}

func TestPayloadStatsAggregatesOneSnapshot(t *testing.T) {
	ctx := context.Background()
	now := time.Unix(1_700_000_000, 0).UTC()
	db := &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row {
		return int64Row{values: []int64{1, 2, 3, 4, 100, 40, 60, 5, 2, 90}}
	}}
	stats, err := newStore(db).PayloadStats(ctx, now)
	if err != nil {
		t.Fatalf("stats: %v", err)
	}
	if stats.ReservedObjects != 1 || stats.ReadyObjects != 2 || stats.ExpiredObjects != 3 ||
		stats.DeletedObjects != 4 || stats.ChargedBytes != 100 || stats.ExpiredBytes != 5 ||
		stats.ReaderLeases != 2 || stats.OldestExpiredBacklog != 90 || stats.ActiveUploads != 1 {
		t.Fatalf("stats = %+v", stats)
	}
	db = &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row {
		return int64Row{err: errors.New("stats")}
	}}
	if _, err := newStore(db).PayloadStats(ctx, now); err == nil {
		t.Fatal("stats failure was ignored")
	}
}
