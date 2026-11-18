package store

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgconn"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// An installed permission keeps the exact signed bindings. A refused update
// reports a regional fence rather than silently acknowledging the renewal.
func TestDeviceSessionStoreReportsFencedAndCurrentPermission(t *testing.T) {
	now := time.Now()
	storeErr := errors.New("database unavailable")
	p := DeviceSession{
		ID: "native", WorkspaceID: "workspace", RegionID: "region", DeviceID: "device",
		KeyFingerprint: "fingerprint", AuthorityRevision: "authority", Revision: 2,
		ExpiresAt: now.Add(time.Minute), CredentialHash: []byte("fixture digest"),
	}
	var updateCount int64
	var execErr error
	db := &fakeDB{t: t, execFn: func(_ context.Context, sql string, args ...any) (pgconn.CommandTag, error) {
		if args[0] != p.ID {
			t.Fatal("wrong session")
		}
		if strings.Contains(sql, "revoked=true") {
			if len(args) != 2 || args[1] != p.Revision {
				t.Fatal("wrong revocation scope")
			}
		} else if len(args) != 9 || args[1] != p.WorkspaceID || args[2] != p.RegionID ||
			args[3] != p.DeviceID || args[4] != p.KeyFingerprint || args[5] != p.AuthorityRevision ||
			args[7] != p.Revision || args[8] != p.ExpiresAt {
			t.Fatal("wrong permit bindings")
		}
		return updateTag(updateCount), execErr
	}}
	s := newStore(db)
	ctx := context.Background()
	if err := s.InstallDeviceSession(ctx, p); !errors.Is(err, ErrPairingFenced) {
		t.Fatalf("refused permit did not preserve terminal fence: %v", err)
	}
	updateCount = 1
	if err := s.InstallDeviceSession(ctx, p); err != nil {
		t.Fatal(err)
	}
	execErr = storeErr
	if err := s.InstallDeviceSession(ctx, p); !errors.Is(err, storeErr) {
		t.Fatalf("install persistence error=%v", err)
	}
	if err := s.RevokeDeviceSession(ctx, p.ID, p.Revision); !errors.Is(err, storeErr) {
		t.Fatalf("revoke persistence error=%v", err)
	}
	db.queryRowFn = func(_ context.Context, sql string, args ...any) pgx.Row {
		switch {
		case strings.Contains(sql, "p.claim_id=$1"):
			return syntheticRow{fill: func(v ...any) { *v[0].(*string) = "terminal" }}
		case strings.Contains(sql, "r.request_id"):
			return syntheticRow{fill: func(v ...any) { *v[0].(*bool) = true }}
		default:
			if len(args) != 2 || args[1] != now {
				t.Fatal("no check against current time")
			}
			return syntheticRow{fill: func(v ...any) {
				*v[0].(*string), *v[1].(*string), *v[2].(*string) = p.ID, p.WorkspaceID, p.RegionID
				*v[3].(*string), *v[4].(*string), *v[5].(*string) = p.DeviceID, p.KeyFingerprint, p.AuthorityRevision
				*v[6].(*int64), *v[7].(*time.Time) = p.Revision, p.ExpiresAt
			}}
		}
	}
	for _, lookup := range []func() (DeviceSession, error){
		func() (DeviceSession, error) { return s.DeviceSessionByCredential(ctx, p.CredentialHash, now) },
		func() (DeviceSession, error) { return s.ActiveDeviceSession(ctx, p.ID, now) },
	} {
		if got, err := lookup(); err != nil || got.ID != p.ID || got.Revision != p.Revision {
			t.Fatalf("lookup=%+v err=%v", got, err)
		}
	}
	if state, err := s.DeviceSessionPairingStatus(ctx, p, "host", "claim"); err != nil || state != "terminal" {
		t.Fatalf("pairing status=%q err=%v", state, err)
	}
	if ended, err := s.SessionCancellationReason(ctx, "request"); err != nil || !ended {
		t.Fatalf("cancellation=%v err=%v", ended, err)
	}
	rows := &fakeRowSet{rows: []pgx.Row{syntheticRow{fill: func(v ...any) { *v[0].(*string) = "host" }}}}
	db.queryFn = func(context.Context, string, ...any) (pgx.Rows, error) { return rows, nil }
	if got, err := s.DeviceSessionHosts(ctx, p); err != nil || !got["host"] || rows.closed != 1 {
		t.Fatalf("host scope=%v err=%v", got, err)
	}
}

// Forwarding checks the authority that originally admitted the request and
// serializes the actual publish with regional revocation.
func TestDeviceSessionStoreFencesQueuedForwardingAndPairing(t *testing.T) {
	var session *string
	id, authority := "session", "authority"
	var kind = int(whalereinv1.RequestKind_REQUEST_KIND_COMMAND)
	var active, granted bool
	var transactionErr error
	tx := &scriptedTx{queryRowFn: func(_ context.Context, sql string, _ ...any) pgx.Row {
		value := active
		if strings.Contains(sql, "grant_authority") {
			value = granted
		}
		return syntheticRow{fill: func(v ...any) { *v[0].(*bool) = value }}
	}, execFn: func(_ context.Context, sql string, args ...any) (pgconn.CommandTag, error) {
		if !strings.Contains(sql, "whalerein.session_authority") || args[0] != authority {
			t.Fatal("incorrect confirmation authority")
		}
		return updateTag(1), nil
	}}
	db := &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row {
		return syntheticRow{fill: func(v ...any) {
			*v[0].(**string), *v[1].(**string) = session, &authority
			*v[2].(*string), *v[3].(*string), *v[4].(*int) = "host", "device", kind
		}}
	}, beginFn: func(context.Context) (pgx.Tx, error) { return tx, transactionErr }}
	s := newStore(db)
	ctx := context.Background()
	publishes := 0
	publish := func() error { publishes++; return nil }
	check := func(ctx context.Context, want error, count int) {
		t.Helper()
		if err := s.PublishDuringDeviceSession(ctx, "request", publish); !errors.Is(err, want) || publishes != count {
			t.Fatalf("forward error=%v publications=%d; want %v and %d", err, publishes, want, count)
		}
	}
	check(ctx, nil, 1)
	session = &id
	check(WithDeviceSessionAuthority(ctx, "new-revision"), ErrDeviceSessionEnded, 1)
	transactionErr = errors.New("no database")
	check(ctx, transactionErr, 1)
	transactionErr = nil
	ctx = WithDeviceSessionAuthority(ctx, authority)
	check(ctx, ErrDeviceSessionEnded, 1)
	active = true
	check(ctx, ErrDeviceSessionEnded, 1)
	granted = true
	check(ctx, nil, 2)
	kind, granted = int(whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE), false
	check(ctx, nil, 3)
	if !tx.rolledBack || tx.committed {
		t.Fatal("the forwarding lock was not released without altering state")
	}
	if err := applyDeviceSessionAuthority(ctx, tx); err != nil {
		t.Fatal(err)
	}
}

// Managed request admission stores the original session in the same
// transaction as the journal entry and rolls back if its fence or write fails.
func TestCreateManagedRequestKeepsSessionAndRequestInOneTransaction(t *testing.T) {
	now := time.Now()
	req := Request{
		RequestID: "request", HostID: "host", DeviceID: "device", DeviceSessionID: "session",
		Status: whalereinv1.OnlineStatus_received, ExpiresAt: now.Add(time.Minute),
	}
	var active bool
	var writeErr, beginErr error
	tx := &scriptedTx{queryRowFn: func(_ context.Context, sql string, args ...any) pgx.Row {
		if strings.Contains(sql, "FOR SHARE") {
			if args[0] != req.DeviceSessionID || args[1] != req.DeviceID || args[3] != "authority" {
				t.Fatal("wrong initial session authority")
			}
			return syntheticRow{fill: func(v ...any) { *v[0].(*bool) = active }}
		}
		if !strings.Contains(sql, "INSERT INTO requests") {
			t.Fatal("the request must be persisted after locking its session")
		}
		return requestFixtureRow([]byte{1})
	}, execFn: func(_ context.Context, sql string, args ...any) (pgconn.CommandTag, error) {
		if !strings.Contains(sql, "device_session_authority") || args[0] != req.RequestID || args[1] != req.DeviceSessionID {
			t.Fatal("incorrect stored session scope")
		}
		return updateTag(1), writeErr
	}}
	db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, beginErr }}
	s := newStore(db)
	ctx := WithDeviceSessionAuthority(context.Background(), "authority")
	if _, err := s.CreateRequest(ctx, req); !errors.Is(err, ErrDeviceSessionEnded) || tx.committed {
		t.Fatalf("inactive session admitted work: %v", err)
	}
	active = true
	writeErr = errors.New("scope update failed")
	if _, err := s.CreateRequest(ctx, req); !errors.Is(err, writeErr) || tx.committed {
		t.Fatalf("a partial request was committed: %v", err)
	}
	writeErr = nil
	if result, err := s.CreateRequest(ctx, req); err != nil || !result.Created || !tx.committed {
		t.Fatalf("new session request could not commit: created=%v, %v", result.Created, err)
	}
	beginErr = errors.New("no transaction")
	if _, err := s.CreateRequest(ctx, req); !errors.Is(err, beginErr) {
		t.Fatalf("failed to open transaction: %v", err)
	}
}

// Managed pairing must keep the same authority, session and operation
// through HELLO and commits only if the exact stored pairing remains eligible.
func TestManagedPairingStoreMaintainsOriginalScopeDuringCommit(t *testing.T) {
	now := time.Now()
	op := &PairingOperation{
		ClaimID: "claim", HostID: "host", DeviceID: "device", CloudSessionID: "session",
		CloudWorkspaceID: "workspace", CloudEnrollmentRevision: "host-revision",
		ExpiresAt: now.Add(time.Minute),
	}
	active, bound, eligible := true, true, true
	var boundWriteError error
	tx := &scriptedTx{queryRowFn: func(_ context.Context, sql string, _ ...any) pgx.Row {
		return syntheticRow{fill: func(v ...any) {
			switch {
			case strings.Contains(sql, "COALESCE"):
				*v[0].(*int64) = 0
			case strings.Contains(sql, "pairing_outcomes") && !strings.Contains(sql, "external_enrollments"):
				*v[0].(*bool) = false
			case strings.Contains(sql, "FROM pairing_tokens"):
				*v[0].(*time.Time), *v[1].(*time.Time) = now, op.ExpiresAt
			case strings.Contains(sql, "FOR SHARE"):
				*v[0].(*bool) = active
			case strings.Contains(sql, "external_enrollments"):
				*v[0].(*bool) = eligible
			default:
				*v[0].(*bool) = bound
			}
		}}
	}, execFn: func(_ context.Context, sql string, args ...any) (pgconn.CommandTag, error) {
		if strings.Contains(sql, "pairing_device_sessions") {
			if args[0] != op.ClaimID || args[1] != op.CloudSessionID || args[2] != op.CloudWorkspaceID {
				t.Fatal("incorrect durable pairing session")
			}
			return updateTag(1), boundWriteError
		}
		return updateTag(1), nil
	}}
	s := newStore(&fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }})
	ctx := WithDeviceSessionAuthority(context.Background(), "authority")
	active = false
	if err := s.SavePairingOperation(ctx, *op); !errors.Is(err, ErrPairingFenced) || tx.committed {
		t.Fatalf("revoked HELLO became durable: %v", err)
	}
	if err := verifyCloudPairing(ctx, tx, op, op.HostID, op.DeviceID); !errors.Is(err, ErrPairingFenced) {
		t.Fatalf("revoked FINISH remained eligible: %v", err)
	}
	active = true
	boundWriteError = errors.New("pairing scope persistence")
	if err := s.SavePairingOperation(ctx, *op); !errors.Is(err, boundWriteError) || tx.committed {
		t.Fatalf("partial pairing saved: %v", err)
	}
	boundWriteError = nil
	if err := s.SavePairingOperation(ctx, *op); err != nil || !tx.committed {
		t.Fatalf("valid device pairing HELLO didn't commit: %v", err)
	}
	bound = false
	if err := verifyCloudPairing(ctx, tx, op, op.HostID, op.DeviceID); !errors.Is(err, ErrPairingFenced) {
		t.Fatalf("missing original bind allowed commit: %v", err)
	}
	bound = true
	if err := verifyCloudPairing(ctx, tx, op, op.HostID, op.DeviceID); err != nil {
		t.Fatalf("eligible matched scope refused: %v", err)
	}
	eligible = false
	if err := verifyCloudPairing(ctx, tx, op, op.HostID, op.DeviceID); !errors.Is(err, ErrPairingFenced) {
		t.Fatalf("disallowed Host permit still accepted: %v", err)
	}
}
