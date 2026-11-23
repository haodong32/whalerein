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

func TestRefusedOperationErrorMapsStableRefusals(t *testing.T) {
	for _, tc := range []struct {
		name    string
		outcome string
		reason  string
		wantErr error
	}{
		{"expired", OperationExpired, "", ErrAuthorizationExpired},
		{"fenced", OperationFenced, "", ErrOperationFenced},
		{"duplicate host", OperationRefused, "duplicate_host", ErrDuplicateHost},
		{"disabled enrollment", OperationRefused, "enrollment_disabled", ErrEnrollmentDisabled},
		{"other refusal", OperationRefused, "revision_mismatch", ErrOperationRefused},
		{"admitted is still pending", OperationAdmitted, "", ErrOperationPending},
		{"unknown outcome is pending", "", "", ErrOperationPending},
	} {
		t.Run(tc.name, func(t *testing.T) {
			got := refusedOperationError(ExternalOperation{Outcome: tc.outcome, Reason: tc.reason})
			if !errors.Is(got, tc.wantErr) {
				t.Fatalf("refusal = %v", got)
			}
		})
	}
}

func TestMarkOperationOutcomeWritesTheDerivedState(t *testing.T) {
	for _, tc := range []struct {
		name      string
		outcome   string
		wantState string
	}{
		{"committed", OperationCommitted, "operation_committed"},
		{"refused", OperationRefused, "operation_refused"},
		{"expired", OperationExpired, "operation_expired"},
		{"fenced", OperationFenced, "operation_fenced"},
		{"in flight", OperationAdmitted, "operation_admitted"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var gotOperationID, gotState, gotOutcome, gotReason string
			tx := &externalTx{execFn: func(_ context.Context, _ string, args ...any) (pgconn.CommandTag, error) {
				gotOperationID, _ = args[0].(string)
				gotState, _ = args[1].(string)
				gotOutcome, _ = args[2].(string)
				gotReason, _ = args[3].(string)
				return updateTag(1), nil
			}}
			if err := markOperationOutcome(context.Background(), tx, "op-1", tc.outcome, "reason"); err != nil {
				t.Fatal(err)
			}
			if gotOperationID != "op-1" || gotState != tc.wantState || gotOutcome != tc.outcome || gotReason != "reason" {
				t.Fatalf("outcome write = %q %q %q %q", gotOperationID, gotState, gotOutcome, gotReason)
			}
		})
	}

	t.Run("storage failure", func(t *testing.T) {
		tx := &externalTx{execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
			return pgconn.CommandTag{}, errStoreUnit
		}}
		err := markOperationOutcome(context.Background(), tx, "op-1", OperationRefused, "reason")
		if !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "record operation outcome") {
			t.Fatalf("outcome error = %v", err)
		}
	})
}

func TestNullableTimeNormalizesPresentValues(t *testing.T) {
	if got := nullableTime(nil); got != nil {
		t.Fatalf("nil time = %v", got)
	}
	local := time.Date(2026, 1, 2, 3, 4, 5, 0, time.FixedZone("offset", 3600))
	got, ok := nullableTime(&local).(time.Time)
	if !ok || got.Location() != time.UTC || !got.Equal(local) {
		t.Fatalf("normalized time = %v", nullableTime(&local))
	}
}

func TestLoadRemovalDistinguishesMissingAndCorruptRows(t *testing.T) {
	at := time.Unix(1_700_000_000, 0).UTC()

	t.Run("missing removal", func(t *testing.T) {
		db := &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row {
			return syntheticRow{err: pgx.ErrNoRows}
		}}
		if _, err := loadRemoval(context.Background(), db, "rem-1"); !errors.Is(err, ErrNotFound) {
			t.Fatalf("missing removal error = %v", err)
		}
	})

	t.Run("storage failure", func(t *testing.T) {
		db := &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row {
			return syntheticRow{err: errStoreUnit}
		}}
		if _, err := loadRemoval(context.Background(), db, "rem-1"); !errors.Is(err, errStoreUnit) {
			t.Fatalf("removal storage error = %v", err)
		}
	})

	t.Run("undecodable revocations", func(t *testing.T) {
		db := &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row {
			return externalRow{values: []any{"rem-1", "host-1", "ws-1", "op-1", "1", at, (*time.Time)(nil), []byte("not-json")}}
		}}
		if _, err := loadRemoval(context.Background(), db, "rem-1"); err == nil || !strings.Contains(err.Error(), "decode removal revocations") {
			t.Fatalf("decode error = %v", err)
		}
	})

	t.Run("stored removal without revocations", func(t *testing.T) {
		db := &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row {
			return externalRow{values: []any{"rem-1", "host-1", "ws-1", "op-1", "2", at, (*time.Time)(nil), []byte(nil)}}
		}}
		removal, err := loadRemoval(context.Background(), db, "rem-1")
		if err != nil || removal.RemovalOperationID != "rem-1" || removal.EnrollmentRevision != "2" {
			t.Fatalf("removal = %+v err=%v", removal, err)
		}
		if removal.Revocations == nil || len(removal.Revocations) != 0 {
			t.Fatalf("revocations = %v", removal.Revocations)
		}
	})
}

func TestLoadLatestRemovalDistinguishesMissingAndCorruptRows(t *testing.T) {
	at := time.Unix(1_700_000_000, 0).UTC()
	enforced := at.Add(time.Minute)

	t.Run("no removal", func(t *testing.T) {
		db := &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row {
			return syntheticRow{err: pgx.ErrNoRows}
		}}
		removal, err := loadLatestRemoval(context.Background(), db, "host-1")
		if err != nil || removal != nil {
			t.Fatalf("latest removal = %+v err=%v", removal, err)
		}
	})

	t.Run("storage failure", func(t *testing.T) {
		db := &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row {
			return syntheticRow{err: errStoreUnit}
		}}
		if _, err := loadLatestRemoval(context.Background(), db, "host-1"); !errors.Is(err, errStoreUnit) {
			t.Fatalf("latest removal error = %v", err)
		}
	})

	t.Run("undecodable revocations", func(t *testing.T) {
		db := &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row {
			return externalRow{values: []any{"rem-1", "host-1", "ws-1", "op-1", "2", at, &enforced, []byte("{")}}
		}}
		if _, err := loadLatestRemoval(context.Background(), db, "host-1"); err == nil || !strings.Contains(err.Error(), "decode removal revocations") {
			t.Fatalf("latest decode error = %v", err)
		}
	})

	t.Run("stored removal", func(t *testing.T) {
		db := &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row {
			return externalRow{values: []any{"rem-1", "host-1", "ws-1", "op-1", "2", at, &enforced, []byte(`{"device-1":4}`)}}
		}}
		removal, err := loadLatestRemoval(context.Background(), db, "host-1")
		if err != nil || removal == nil || removal.RemovalOperationID != "rem-1" ||
			removal.EnforcedAt == nil || removal.Revocations["device-1"] != 4 {
			t.Fatalf("latest removal = %+v err=%v", removal, err)
		}
	})
}

func TestApplyRemovalDerivesTheRemovalState(t *testing.T) {
	enforced := time.Unix(1_700_000_100, 0).UTC()
	disabledAt := enforced.Add(-time.Minute)
	for _, tc := range []struct {
		name        string
		enrollment  ExternalEnrollment
		removal     *removalRow
		wantState   string
		wantRemoval string
	}{
		{"enforced", ExternalEnrollment{State: "enrollment_disabled"}, &removalRow{RemovalOperationID: "rem-1", EnforcedAt: &enforced}, "enforced", "rem-1"},
		{"pending", ExternalEnrollment{State: "enrollment_disabled"}, &removalRow{RemovalOperationID: "rem-2"}, "pending", "rem-2"},
		{"disabled by state", ExternalEnrollment{State: "enrollment_disabled"}, nil, "disabled", ""},
		{"disabled by timestamp", ExternalEnrollment{DisabledAt: &disabledAt}, nil, "disabled", ""},
		{"never removed", ExternalEnrollment{State: "enrollment_ready"}, nil, "none", ""},
	} {
		t.Run(tc.name, func(t *testing.T) {
			enrollment := tc.enrollment
			applyRemoval(&enrollment, tc.removal)
			if enrollment.RemovalState != tc.wantState || enrollment.RemovalOperationID != tc.wantRemoval {
				t.Fatalf("state=%q removal=%q", enrollment.RemovalState, enrollment.RemovalOperationID)
			}
			if tc.removal != nil && tc.removal.EnforcedAt != nil && enrollment.RemovalEnforcedAt == nil {
				t.Fatal("enforced timestamp was not copied")
			}
		})
	}
}

func TestDisabledFromRemovalDefaultsTheRevocationMap(t *testing.T) {
	at := time.Unix(1_700_000_000, 0).UTC()
	removal := removalRow{
		RemovalOperationID: "rem-1", HostID: "host-1", WorkspaceID: "ws-1",
		EnrollmentOperationID: "op-1", EnrollmentRevision: "2", DisabledAt: at,
	}
	disabled := disabledFromRemoval(removal)
	if disabled.Revocations == nil || len(disabled.Revocations) != 0 {
		t.Fatalf("revocations = %v", disabled.Revocations)
	}
	if disabled.HostID != "host-1" || disabled.RemovalOperationID != "rem-1" ||
		disabled.Target != (RemovalTarget{WorkspaceID: "ws-1", EnrollmentOperationID: "op-1", EnrollmentRevision: "2"}) {
		t.Fatalf("disabled removal = %+v", disabled)
	}

	removal.Revocations = map[string]uint64{"device-1": 4}
	disabled = disabledFromRemoval(removal)
	if disabled.Revocations["device-1"] != 4 {
		t.Fatalf("stored revocations = %v", disabled.Revocations)
	}
}

func TestConsumeHostSignatureRejectsMalformedInputAndStorageFailures(t *testing.T) {
	at := time.Unix(1_700_000_000, 0).UTC()
	hash := make([]byte, 32)

	t.Run("no database", func(t *testing.T) {
		var store *Store
		if _, err := store.ConsumeHostSignature(context.Background(), hash, "host-1", "purpose", "device-1", at.UnixMilli(), at); err == nil || !strings.Contains(err.Error(), "no database pool") {
			t.Fatalf("nil store error = %v", err)
		}
	})

	t.Run("short signature hash", func(t *testing.T) {
		db := &fakeDB{t: t}
		if _, err := newStore(db).ConsumeHostSignature(context.Background(), hash[:31], "host-1", "purpose", "device-1", at.UnixMilli(), at); err == nil || !strings.Contains(err.Error(), "32 bytes") {
			t.Fatalf("short hash error = %v", err)
		}
	})

	t.Run("missing identity", func(t *testing.T) {
		db := &fakeDB{t: t}
		if _, err := newStore(db).ConsumeHostSignature(context.Background(), hash, "", "purpose", "device-1", at.UnixMilli(), at); err == nil || !strings.Contains(err.Error(), "host id and purpose are required") {
			t.Fatalf("missing host error = %v", err)
		}
		if _, err := newStore(db).ConsumeHostSignature(context.Background(), hash, "host-1", "", "device-1", at.UnixMilli(), at); err == nil || !strings.Contains(err.Error(), "host id and purpose are required") {
			t.Fatalf("missing purpose error = %v", err)
		}
	})

	t.Run("prune failure", func(t *testing.T) {
		db := &fakeDB{t: t, execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
			return pgconn.CommandTag{}, errStoreUnit
		}}
		if _, err := newStore(db).ConsumeHostSignature(context.Background(), hash, "host-1", "purpose", "device-1", at.UnixMilli(), time.Time{}); !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "prune host signatures") {
			t.Fatalf("prune error = %v", err)
		}
	})

	t.Run("insert failure", func(t *testing.T) {
		db := &fakeDB{t: t, execFn: func(_ context.Context, sql string, _ ...any) (pgconn.CommandTag, error) {
			if strings.Contains(sql, "DELETE FROM host_signature_replays") {
				return deleteTag(0), nil
			}
			return pgconn.CommandTag{}, errStoreUnit
		}}
		if _, err := newStore(db).ConsumeHostSignature(context.Background(), hash, "host-1", "purpose", "device-1", at.UnixMilli(), at); !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "consume host signature") {
			t.Fatalf("insert error = %v", err)
		}
	})
}

func TestConsumeHostProofNonceRejectsMissingPurposeAndStorageFailures(t *testing.T) {
	at := time.Unix(1_700_000_000, 0).UTC()
	nonce := make([]byte, 32)

	t.Run("missing purpose", func(t *testing.T) {
		db := &fakeDB{t: t}
		if _, err := newStore(db).ConsumeHostProofNonce(context.Background(), nonce, "", at); err == nil || !strings.Contains(err.Error(), "purpose is required") {
			t.Fatalf("purpose error = %v", err)
		}
	})

	t.Run("prune failure", func(t *testing.T) {
		db := &fakeDB{t: t, execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
			return pgconn.CommandTag{}, errStoreUnit
		}}
		if _, err := newStore(db).ConsumeHostProofNonce(context.Background(), nonce, "enrollment_status", at); !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "prune host proof nonces") {
			t.Fatalf("prune error = %v", err)
		}
	})

	t.Run("insert failure", func(t *testing.T) {
		db := &fakeDB{t: t, execFn: func(_ context.Context, sql string, _ ...any) (pgconn.CommandTag, error) {
			if strings.Contains(sql, "DELETE FROM host_proof_nonces") {
				return deleteTag(0), nil
			}
			return pgconn.CommandTag{}, errStoreUnit
		}}
		if _, err := newStore(db).ConsumeHostProofNonce(context.Background(), nonce, "enrollment_status", at); !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "consume host proof nonce") {
			t.Fatalf("insert error = %v", err)
		}
	})
}

func TestDeviceSessionContextHelpersBindOnlyNonEmptyValues(t *testing.T) {
	base := context.Background()
	if got := WithRequestDeviceSession(base, ""); got != base {
		t.Fatal("an empty request session must leave the context unchanged")
	}
	withSession := WithRequestDeviceSession(base, "session-1")
	if withSession == base || withSession.Value(requestSessionKey{}) != "session-1" {
		t.Fatalf("request session context = %v", withSession.Value(requestSessionKey{}))
	}

	if got := WithDeviceSessionAuthority(base, ""); got != base {
		t.Fatal("an empty authority must leave the context unchanged")
	}
	withAuthority := WithDeviceSessionAuthority(base, "authority-1")
	if withAuthority == base || withAuthority.Value(sessionAuthorityKey{}) != "authority-1" {
		t.Fatalf("authority context = %v", withAuthority.Value(sessionAuthorityKey{}))
	}
}
