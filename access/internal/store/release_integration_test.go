//go:build integration

package store_test

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"sync"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
	"github.com/whalerein/whalerein/access/internal/store/storetest"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

const releaseDeliveryLimit = 1 << 20

func admitStampedRequest(t *testing.T, s *store.Store, hostID, deviceID, requestID string, kind whalereinv1.RequestKind, stamp *store.GrantStamp, expiresAt time.Time) store.Request {
	t.Helper()
	ctx := context.Background()
	row := store.Request{
		RequestID:        requestID,
		HostID:           hostID,
		DeviceID:         deviceID,
		Kind:             kind,
		ProtocolVersion:  1,
		Status:           whalereinv1.OnlineStatus_received,
		RequestBody:      []byte("request-ciphertext"),
		ExpiresAt:        expiresAt,
		ForwardState:     store.ForwardNone,
		ClientLeaseState: store.ClientLeaseClosed,
		Stamp:            stamp,
	}
	if kind == whalereinv1.RequestKind_REQUEST_KIND_COMMAND {
		row.CommandID = "command-" + requestID
		row.PayloadDigest = []byte("digest-" + requestID)
	}
	outcome, err := s.CreateRequest(ctx, row)
	if err != nil || !outcome.Created {
		t.Fatalf("admit request: outcome=%+v err=%v", outcome, err)
	}
	return outcome.Request
}

func storeSucceededResult(t *testing.T, s *store.Store, hostID, deviceID, requestID string, body []byte, at time.Time) {
	t.Helper()
	applied, err := s.ApplyHostResult(context.Background(), store.HostResultUpdate{
		RequestID: requestID, HostID: hostID, DeviceID: deviceID,
		Status: whalereinv1.OnlineStatus_succeeded, ResultBody: body, At: at,
	})
	if err != nil || !applied {
		t.Fatalf("store result: applied=%v err=%v", applied, err)
	}
}

func currentStamp(t *testing.T, s *store.Store, hostID, deviceID string, rawLimit int64) *store.GrantStamp {
	t.Helper()
	grant, err := s.AdmissionGrantFor(context.Background(), hostID, deviceID)
	if err != nil {
		t.Fatal(err)
	}
	return &store.GrantStamp{
		GrantGeneration:            grant.GrantGeneration,
		RevocationGeneration:       grant.RevocationGeneration,
		DeviceRevocationGeneration: grant.DeviceRevocationGeneration,
		ResultLimitBytes:           rawLimit,
	}
}

// When a stored result is read, the release decision requires the exact durable grant and revocation revisions captured at admission, so a revoke or re-grant fences uncollected ciphertext while preserving command metadata.
func TestReleaseRequestFencesStoredCiphertextByDurableRevision(t *testing.T) {
	t.Run("stored and current", func(t *testing.T) {
		s, _ := storetest.New(t)
		seedDevice(t, s, "host", "device")
		now := time.Now().UTC()
		row := admitStampedRequest(t, s, "host", "device", "r1", whalereinv1.RequestKind_REQUEST_KIND_QUERY, currentStamp(t, s, "host", "device", releaseDeliveryLimit), now.Add(time.Hour))
		storeSucceededResult(t, s, "host", "device", row.RequestID, []byte("ciphertext"), now)

		outcome, err := s.ReleaseRequest(context.Background(), store.ReleaseScope{DeviceID: "device", RequestID: "r1", DeliveryLimitBytes: releaseDeliveryLimit})
		if err != nil {
			t.Fatal(err)
		}
		if !outcome.SessionValid || !outcome.StampPresent || !outcome.GrantActive || !outcome.StampCurrent ||
			outcome.BodiesCleared || !outcome.WithinResultBound || !outcome.WithinDelivery {
			t.Fatalf("current release was not authorized: %+v", outcome)
		}
		if string(outcome.Request.ResultBody) != "ciphertext" {
			t.Fatalf("ciphertext = %q", outcome.Request.ResultBody)
		}
	})

	t.Run("stored before revoke first and repeat get", func(t *testing.T) {
		s, _ := storetest.New(t)
		seedDevice(t, s, "host", "device")
		now := time.Now().UTC()
		admitStampedRequest(t, s, "host", "device", "r1", whalereinv1.RequestKind_REQUEST_KIND_QUERY, currentStamp(t, s, "host", "device", releaseDeliveryLimit), now.Add(time.Hour))
		storeSucceededResult(t, s, "host", "device", "r1", []byte("ciphertext"), now)
		if err := s.RevokeGrant(context.Background(), "host", "device", now.Add(time.Second)); err != nil {
			t.Fatal(err)
		}
		for attempt := 0; attempt < 2; attempt++ {
			outcome, err := s.ReleaseRequest(context.Background(), store.ReleaseScope{DeviceID: "device", RequestID: "r1", DeliveryLimitBytes: releaseDeliveryLimit})
			if err != nil {
				t.Fatal(err)
			}
			if outcome.GrantActive || outcome.StampCurrent {
				t.Fatalf("revoked ciphertext was authorized on attempt %d: %+v", attempt, outcome)
			}
			if string(outcome.Request.ResultBody) != "ciphertext" {
				t.Fatal("stored ciphertext was lost, not fenced")
			}
		}
	})

	t.Run("stays fenced after regrant while new result is authorized", func(t *testing.T) {
		s, _ := storetest.New(t)
		seedDevice(t, s, "host", "device")
		now := time.Now().UTC()
		old := admitStampedRequest(t, s, "host", "device", "old", whalereinv1.RequestKind_REQUEST_KIND_QUERY, currentStamp(t, s, "host", "device", releaseDeliveryLimit), now.Add(time.Hour))
		storeSucceededResult(t, s, "host", "device", old.RequestID, []byte("old-ciphertext"), now)
		if err := s.RevokeGrant(context.Background(), "host", "device", now.Add(time.Second)); err != nil {
			t.Fatal(err)
		}
		if err := s.CreatePairedDevice(context.Background(), "host", "device", "fp", []byte("auth-device"), now.Add(2*time.Second)); err != nil {
			t.Fatal(err)
		}
		outcome, err := s.ReleaseRequest(context.Background(), store.ReleaseScope{DeviceID: "device", RequestID: "old", DeliveryLimitBytes: releaseDeliveryLimit})
		if err != nil {
			t.Fatal(err)
		}
		if !outcome.GrantActive || outcome.StampCurrent {
			t.Fatalf("old result was refreshed by a regrant: %+v", outcome)
		}

		fresh := admitStampedRequest(t, s, "host", "device", "new", whalereinv1.RequestKind_REQUEST_KIND_QUERY, currentStamp(t, s, "host", "device", releaseDeliveryLimit), now.Add(time.Hour))
		storeSucceededResult(t, s, "host", "device", fresh.RequestID, []byte("new-ciphertext"), now.Add(3*time.Second))
		outcome, err = s.ReleaseRequest(context.Background(), store.ReleaseScope{DeviceID: "device", RequestID: "new", DeliveryLimitBytes: releaseDeliveryLimit})
		if err != nil {
			t.Fatal(err)
		}
		if !outcome.StampCurrent || !outcome.GrantActive || string(outcome.Request.ResultBody) != "new-ciphertext" {
			t.Fatalf("new result was not authorized: %+v", outcome)
		}
	})

	t.Run("missing stamp fails closed", func(t *testing.T) {
		s, _ := storetest.New(t)
		seedDevice(t, s, "host", "device")
		now := time.Now().UTC()
		if _, err := s.Pool().Exec(context.Background(), `INSERT INTO requests
			(request_id,host_id,device_id,kind,status,result_body,expires_at,forward_state,created_at,updated_at)
			VALUES ('legacy','host','device',2,'succeeded',$1,$2,'none',$3,$3)`,
			[]byte("legacy-ciphertext"), now.Add(time.Hour), now); err != nil {
			t.Fatal(err)
		}
		outcome, err := s.ReleaseRequest(context.Background(), store.ReleaseScope{DeviceID: "device", RequestID: "legacy", DeliveryLimitBytes: releaseDeliveryLimit})
		if err != nil {
			t.Fatal(err)
		}
		if outcome.StampPresent || outcome.StampCurrent || outcome.WithinResultBound {
			t.Fatalf("unstamped row was authorized: %+v", outcome)
		}
		if string(outcome.Request.ResultBody) != "legacy-ciphertext" {
			t.Fatal("unstamped ciphertext is not retained for the fencing decision")
		}
	})

	t.Run("foreign device and wrong request id", func(t *testing.T) {
		s, _ := storetest.New(t)
		seedDevice(t, s, "host", "device")
		seedDevice(t, s, "host", "other")
		now := time.Now().UTC()
		admitStampedRequest(t, s, "host", "device", "r1", whalereinv1.RequestKind_REQUEST_KIND_QUERY, currentStamp(t, s, "host", "device", releaseDeliveryLimit), now.Add(time.Hour))
		if _, err := s.ReleaseRequest(context.Background(), store.ReleaseScope{DeviceID: "other", RequestID: "r1"}); !errors.Is(err, store.ErrNotFound) {
			t.Fatalf("foreign device = %v", err)
		}
		if _, err := s.ReleaseRequest(context.Background(), store.ReleaseScope{DeviceID: "device", RequestID: "missing"}); !errors.Is(err, store.ErrNotFound) {
			t.Fatalf("wrong request id = %v", err)
		}
	})

	t.Run("foreign host has no current authority", func(t *testing.T) {
		s, _ := storetest.New(t)
		seedDevice(t, s, "host", "device")
		if _, err := s.EnsureHost(context.Background(), "other-host"); err != nil {
			t.Fatal(err)
		}
		now := time.Now().UTC()
		stamp := currentStamp(t, s, "host", "device", releaseDeliveryLimit)
		admitStampedRequest(t, s, "other-host", "device", "r1", whalereinv1.RequestKind_REQUEST_KIND_QUERY, stamp, now.Add(time.Hour))
		storeSucceededResult(t, s, "other-host", "device", "r1", []byte("ciphertext"), now)
		outcome, err := s.ReleaseRequest(context.Background(), store.ReleaseScope{DeviceID: "device", RequestID: "r1", DeliveryLimitBytes: releaseDeliveryLimit})
		if err != nil {
			t.Fatal(err)
		}
		if outcome.GrantActive || outcome.StampCurrent {
			t.Fatalf("foreign host authority was accepted: %+v", outcome)
		}
	})

	t.Run("lowered delivery cap and stored bound", func(t *testing.T) {
		s, _ := storetest.New(t)
		seedDevice(t, s, "host", "device")
		now := time.Now().UTC()
		admitStampedRequest(t, s, "host", "device", "r1", whalereinv1.RequestKind_REQUEST_KIND_QUERY, currentStamp(t, s, "host", "device", releaseDeliveryLimit), now.Add(time.Hour))
		storeSucceededResult(t, s, "host", "device", "r1", []byte("0123456789"), now)
		outcome, err := s.ReleaseRequest(context.Background(), store.ReleaseScope{DeviceID: "device", RequestID: "r1", DeliveryLimitBytes: 5})
		if err != nil {
			t.Fatal(err)
		}
		if outcome.WithinDelivery || !outcome.WithinResultBound {
			t.Fatalf("lowered GET cap was not applied: %+v", outcome)
		}
		if _, err := s.Pool().Exec(context.Background(), `UPDATE requests SET result_limit_bytes=4 WHERE request_id='r1'`); err != nil {
			t.Fatal(err)
		}
		outcome, err = s.ReleaseRequest(context.Background(), store.ReleaseScope{DeviceID: "device", RequestID: "r1", DeliveryLimitBytes: releaseDeliveryLimit})
		if err != nil {
			t.Fatal(err)
		}
		if outcome.WithinResultBound {
			t.Fatalf("stored result bound was not applied: %+v", outcome)
		}
	})

	t.Run("cleared body is never released", func(t *testing.T) {
		s, _ := storetest.New(t)
		seedDevice(t, s, "host", "device")
		now := time.Now().UTC()
		admitStampedRequest(t, s, "host", "device", "r1", whalereinv1.RequestKind_REQUEST_KIND_QUERY, currentStamp(t, s, "host", "device", releaseDeliveryLimit), now.Add(time.Hour))
		storeSucceededResult(t, s, "host", "device", "r1", []byte("ciphertext"), now)
		if _, err := s.Pool().Exec(context.Background(), `UPDATE requests SET result_body=''::bytea,bodies_cleared=true WHERE request_id='r1'`); err != nil {
			t.Fatal(err)
		}
		outcome, err := s.ReleaseRequest(context.Background(), store.ReleaseScope{DeviceID: "device", RequestID: "r1", DeliveryLimitBytes: releaseDeliveryLimit})
		if err != nil {
			t.Fatal(err)
		}
		if !outcome.BodiesCleared || len(outcome.Request.ResultBody) != 0 {
			t.Fatalf("cleared body was released: %+v", outcome)
		}
	})

	t.Run("command metadata survives a fenced body", func(t *testing.T) {
		s, _ := storetest.New(t)
		seedDevice(t, s, "host", "device")
		now := time.Now().UTC()
		stamp := currentStamp(t, s, "host", "device", releaseDeliveryLimit)
		admitStampedRequest(t, s, "host", "device", "c1", whalereinv1.RequestKind_REQUEST_KIND_COMMAND, stamp, now.Add(time.Hour))
		storeSucceededResult(t, s, "host", "device", "c1", []byte("command-ciphertext"), now)
		if err := s.RevokeGrant(context.Background(), "host", "device", now.Add(time.Second)); err != nil {
			t.Fatal(err)
		}
		outcome, err := s.ReleaseRequest(context.Background(), store.ReleaseScope{DeviceID: "device", RequestID: "c1", DeliveryLimitBytes: releaseDeliveryLimit})
		if err != nil {
			t.Fatal(err)
		}
		req := outcome.Request
		if req.Status != whalereinv1.OnlineStatus_succeeded || req.CommandID != "command-c1" ||
			string(req.PayloadDigest) != "digest-c1" || string(req.ResultBody) != "command-ciphertext" {
			t.Fatalf("durable command outcome changed in storage: %+v", req)
		}
		if outcome.StampCurrent || outcome.GrantActive {
			t.Fatal("fenced command unexpectedly authorized")
		}
	})
}

// When a revocation races a result read, the release decision fails closed and never returns ciphertext after the revocation commits.
func TestReleaseRequestRaceWithRevocationIsFailClosed(t *testing.T) {
	s, _ := storetest.New(t)
	seedDevice(t, s, "host", "device")
	ctx := context.Background()
	now := time.Now().UTC()
	// Materialize one result per iteration so a concurrent revoke and release
	// contend over real rows rather than a reused decision.
	for i := 0; i < 12; i++ {
		id := "race-" + time.Now().UTC().Format("150405.000000000")
		stamp := currentStamp(t, s, "host", "device", releaseDeliveryLimit)
		admitStampedRequest(t, s, "host", "device", id, whalereinv1.RequestKind_REQUEST_KIND_QUERY, stamp, now.Add(time.Hour))
		storeSucceededResult(t, s, "host", "device", id, []byte("ciphertext-"+id), now)

		start := make(chan struct{})
		var wg sync.WaitGroup
		var revokedAt, releaseStart time.Time
		var releaseOutcome store.ReleaseOutcome
		var releaseErr error
		wg.Add(2)
		go func() {
			defer wg.Done()
			<-start
			releaseStart = time.Now()
			releaseOutcome, releaseErr = s.ReleaseRequest(ctx, store.ReleaseScope{DeviceID: "device", RequestID: id, DeliveryLimitBytes: releaseDeliveryLimit})
		}()
		go func() {
			defer wg.Done()
			<-start
			if err := s.RevokeGrant(ctx, "host", "device", time.Now().UTC()); err != nil {
				t.Errorf("revoke: %v", err)
			}
			revokedAt = time.Now()
		}()
		close(start)
		wg.Wait()
		if releaseErr != nil {
			t.Fatalf("concurrent release: %v", releaseErr)
		}
		if releaseStart.After(revokedAt) && releaseOutcome.GrantActive && releaseOutcome.StampCurrent {
			t.Fatalf("release started after the revocation commit still authorized ciphertext: %+v", releaseOutcome)
		}
		// Once both finish, the committed revocation always fences the row,
		// whether or not that particular read raced ahead of the commit.
		fenced, err := s.ReleaseRequest(ctx, store.ReleaseScope{DeviceID: "device", RequestID: id, DeliveryLimitBytes: releaseDeliveryLimit})
		if err != nil {
			t.Fatal(err)
		}
		if fenced.GrantActive || fenced.StampCurrent {
			t.Fatalf("post-race release authorized revoked ciphertext: %+v", fenced)
		}
		if err := s.CreatePairedDevice(ctx, "host", "device", "fp", []byte("auth-device"), now.Add(time.Second)); err != nil {
			t.Fatal(err)
		}
	}
}

// When an encrypted request is admitted without a complete durable grant stamp, creation is rejected before it can be forwarded.
func TestCreateRequestRequiresGrantStampForEncryptedKinds(t *testing.T) {
	s, _ := storetest.New(t)
	seedDevice(t, s, "host", "device")
	now := time.Now().UTC()
	for _, kind := range []whalereinv1.RequestKind{
		whalereinv1.RequestKind_REQUEST_KIND_QUERY,
		whalereinv1.RequestKind_REQUEST_KIND_COMMAND,
	} {
		row := newRequest("missing-"+kind.String(), "host", "device", "command", []byte("digest"), whalereinv1.OnlineStatus_received, store.ForwardPending, now)
		row.Kind = kind
		row.Stamp = nil
		if _, err := s.CreateRequest(context.Background(), row); !errors.Is(err, store.ErrMissingGrantStamp) {
			t.Fatalf("%s without stamp = %v", kind, err)
		}
	}
	for _, kind := range []whalereinv1.RequestKind{
		whalereinv1.RequestKind_REQUEST_KIND_HANDSHAKE,
		whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE,
	} {
		row := newRequest("legacy-"+kind.String(), "host", "device", "", nil, whalereinv1.OnlineStatus_received, store.ForwardNone, now)
		row.Kind = kind
		row.Stamp = nil
		if _, err := s.CreateRequest(context.Background(), row); err != nil {
			t.Fatalf("%s without stamp must stay legal: %v", kind, err)
		}
	}
}

// When migration 019 upgrades a database with retained request rows, the new stamp columns stay empty and previously stored ciphertext is fenced rather than backfilled.
func TestMigration019FencesRetainedCiphertextWithoutBackfill(t *testing.T) {
	s := openStoreAtMigration(t, "018_device_session_authority.sql")
	ctx := context.Background()
	now := time.Now().UTC()
	seedDevice(t, s, "host", "device")
	if _, err := s.Pool().Exec(ctx, `INSERT INTO requests
		(request_id,host_id,device_id,kind,status,result_body,expires_at,forward_state,created_at,updated_at)
		VALUES ('retained','host','device',2,'succeeded',$1,$2,'none',$3,$3)`,
		[]byte("retained-ciphertext"), now.Add(time.Hour), now); err != nil {
		t.Fatal(err)
	}
	if err := s.Migrate(ctx); err != nil {
		t.Fatal(err)
	}
	var grantGeneration, revocationGeneration, deviceRevocationGeneration, resultLimit *int64
	if err := s.Pool().QueryRow(ctx, `SELECT grant_generation,revocation_generation,device_revocation_generation,result_limit_bytes
		FROM requests WHERE request_id='retained'`).Scan(&grantGeneration, &revocationGeneration, &deviceRevocationGeneration, &resultLimit); err != nil {
		t.Fatal(err)
	}
	if grantGeneration != nil || revocationGeneration != nil || deviceRevocationGeneration != nil || resultLimit != nil {
		t.Fatal("migration backfilled a grant stamp for a retained row")
	}
	outcome, err := s.ReleaseRequest(ctx, store.ReleaseScope{DeviceID: "device", RequestID: "retained", DeliveryLimitBytes: releaseDeliveryLimit})
	if err != nil {
		t.Fatal(err)
	}
	if outcome.StampPresent || outcome.StampCurrent || outcome.WithinResultBound {
		t.Fatalf("retained ciphertext became releasable after migration: %+v", outcome)
	}
	if string(outcome.Request.ResultBody) != "retained-ciphertext" {
		t.Fatal("retained ciphertext was not preserved")
	}
}

// When an admitted read-only workload with retained result bodies stops and its requests expire, bounded cleanup returns the retained result rows and ciphertext bytes to their pre-test levels while preserving request identity.
func TestRetainedResultDrainReturnsToItsInitialLevel(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	now := time.Now().UTC()
	seedDevice(t, s, "drain-host", "drain-device")
	countRetained := func() (int64, int64) {
		var rows, bytes int64
		if err := s.Pool().QueryRow(ctx, `
			SELECT count(*), COALESCE(sum(octet_length(result_body)), 0)
			FROM requests WHERE result_body <> ''::bytea`).Scan(&rows, &bytes); err != nil {
			t.Fatal(err)
		}
		return rows, bytes
	}
	beforeRows, beforeBytes := countRetained()
	stamp := &store.GrantStamp{GrantGeneration: 1, RevocationGeneration: 1, DeviceRevocationGeneration: 1, ResultLimitBytes: 1 << 20}
	const workload = 8
	for index := 0; index < workload; index++ {
		row := newRequest(fmt.Sprintf("drain-%d", index), "drain-host", "drain-device", "", []byte("digest"), whalereinv1.OnlineStatus_succeeded, store.ForwardNone, now)
		row.Kind = whalereinv1.RequestKind_REQUEST_KIND_QUERY
		row.Stamp = stamp
		row.ExpiresAt = now.Add(time.Second)
		row.ResultBody = bytes.Repeat([]byte{0x5a}, 64*1024)
		if _, err := s.CreateRequest(ctx, row); err != nil {
			t.Fatal(err)
		}
	}
	rows, ciphertext := countRetained()
	if rows != beforeRows+workload || ciphertext <= beforeBytes {
		t.Fatalf("workload was not admitted: rows=%d bytes=%d", rows, ciphertext)
	}
	// A bounded cleanup pass after the retention window must drain the
	// test-owned ciphertext without deleting durable identities.
	cleared, err := s.ClearExpiredBodies(ctx, now.Add(time.Minute))
	if err != nil || cleared != workload {
		t.Fatalf("cleared = %d err=%v", cleared, err)
	}
	afterRows, afterBytes := countRetained()
	if afterRows != beforeRows || afterBytes != beforeBytes {
		t.Fatalf("retained result did not drain: rows=%d bytes=%d want rows=%d bytes=%d", afterRows, afterBytes, beforeRows, beforeBytes)
	}
	var stored int64
	if err := s.Pool().QueryRow(ctx, `SELECT count(*) FROM requests WHERE host_id = 'drain-host'`).Scan(&stored); err != nil {
		t.Fatal(err)
	}
	if stored != workload {
		t.Fatalf("cleanup removed durable request identities: %d", stored)
	}
}
