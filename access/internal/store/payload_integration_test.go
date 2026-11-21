//go:build integration

package store_test

import (
	"context"
	"crypto/sha256"
	"fmt"
	"sync"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
	"github.com/whalerein/whalerein/access/internal/store/storetest"
)

func payloadDigest(seed byte) []byte {
	digest := sha256.Sum256([]byte{seed})
	return digest[:]
}

func payloadReservation(index int, bytes int64) store.PayloadReservation {
	return store.PayloadReservation{
		ObjectID:      fmt.Sprintf("%032x", index),
		NodeID:        "node-1",
		OwnerID:       "node-1",
		HostID:        fmt.Sprintf("host-%d", index%3),
		RequestID:     fmt.Sprintf("request-%d", index),
		Purpose:       "result",
		Length:        bytes,
		ReservedBytes: bytes,
		Digest:        payloadDigest(byte(index)),
		ExpiresAt:     time.Now().UTC().Add(time.Hour),
	}
}

func payloadLimits() store.PayloadLimits {
	return store.PayloadLimits{
		MaxObjectBytes:   100,
		MaxHostBytes:     500,
		MaxNodeBytes:     500,
		MaxObjects:       10,
		MaxActiveUploads: 10,
	}
}

// Concurrent reservations must serialize on the node/host accounting locks
// instead of reading the same totals and overcommitting the limit.
func TestPayloadConcurrentAdmissionDoesNotOvercommit(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	const workers = 24
	const charge = int64(100)
	var wait sync.WaitGroup
	results := make(chan store.PayloadAdmission, workers)
	errs := make(chan error, workers)
	for i := 0; i < workers; i++ {
		wait.Add(1)
		go func(index int) {
			defer wait.Done()
			admission, err := s.ReservePayload(ctx, payloadReservation(index, charge), payloadLimits())
			if err != nil {
				errs <- err
				return
			}
			results <- admission
		}(i)
	}
	wait.Wait()
	close(results)
	close(errs)
	for err := range errs {
		t.Fatalf("reserve: %v", err)
	}
	admitted := 0
	for result := range results {
		if result.Reason == "" {
			admitted++
		}
	}
	stats, err := s.PayloadStats(ctx, time.Now().UTC())
	if err != nil {
		t.Fatal(err)
	}
	if stats.ChargedBytes > payloadLimits().MaxNodeBytes {
		t.Fatalf("charged bytes = %d over the node limit", stats.ChargedBytes)
	}
	if admitted > 5 {
		t.Fatalf("admitted = %d, node limit allows at most 5", admitted)
	}
	if admitted == 0 {
		t.Fatal("no reservation was admitted")
	}
}

// Count and active-upload bounds are checked inside the same admission
// transaction, and a completed upload stops counting as active.
func TestPayloadObjectAndActiveUploadLimits(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	limits := payloadLimits()
	limits.MaxObjects = 2
	limits.MaxActiveUploads = 1
	first, err := s.ReservePayload(ctx, payloadReservation(1, 10), limits)
	if err != nil || first.Reason != "" {
		t.Fatalf("first admission = %+v err=%v", first, err)
	}
	second, err := s.ReservePayload(ctx, payloadReservation(2, 10), limits)
	if err != nil || second.Reason != store.PayloadRefusedCapacity {
		t.Fatalf("second admission = %+v err=%v, want active-upload refusal", second, err)
	}
	now := time.Now().UTC()
	if _, err := s.ConfirmPayloadReady(ctx, first.Object.ObjectID, 10, payloadDigest(1), now); err != nil {
		t.Fatalf("confirm: %v", err)
	}
	third, err := s.ReservePayload(ctx, payloadReservation(2, 10), limits)
	if err != nil || third.Reason != "" {
		t.Fatalf("third admission = %+v err=%v, want admission after ready", third, err)
	}
	fourth, err := s.ReservePayload(ctx, payloadReservation(3, 10), limits)
	if err != nil || fourth.Reason != store.PayloadRefusedCapacity {
		t.Fatalf("fourth admission = %+v err=%v, want object-count refusal", fourth, err)
	}
}

// A host quota is enforced independently of the node total.
func TestPayloadHostQuotaIsEnforced(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	limits := payloadLimits()
	limits.MaxHostBytes = 100
	first, err := s.ReservePayload(ctx, payloadReservation(1, 100), limits)
	if err != nil || first.Reason != "" {
		t.Fatalf("first = %+v err=%v", first, err)
	}
	secondReservation := payloadReservation(2, 1)
	secondReservation.HostID = first.Object.HostID
	second, err := s.ReservePayload(ctx, secondReservation, limits)
	if err != nil || second.Reason != store.PayloadRefusedQuota {
		t.Fatalf("second = %+v err=%v, want host quota refusal", second, err)
	}
}

// A ready object is readable only through a lease. Repeating release cannot
// decrement the reader count twice.
func TestPayloadReaderLeaseIsIdempotentAndCounted(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	now := time.Now().UTC()
	admission, err := s.ReservePayload(ctx, payloadReservation(1, 10), payloadLimits())
	if err != nil || admission.Reason != "" {
		t.Fatalf("admission = %+v err=%v", admission, err)
	}
	if _, err := s.ConfirmPayloadReady(ctx, admission.Object.ObjectID, 10, payloadDigest(1), now); err != nil {
		t.Fatal(err)
	}
	scope := store.PayloadReadScope{ObjectID: admission.Object.ObjectID, HostID: admission.Object.HostID}
	object, err := s.OpenPayloadRead(ctx, scope, "0123456789abcdef0123456789abcdef", now)
	if err != nil || object.ReaderCount != 1 {
		t.Fatalf("open = %+v err=%v", object, err)
	}
	if _, err := s.OpenPayloadRead(ctx, scope, "fedcba9876543210fedcba9876543210", now); err != nil {
		t.Fatalf("second open: %v", err)
	}
	for i := 0; i < 2; i++ {
		if err := s.ReleasePayloadRead(ctx, object.ObjectID, "0123456789abcdef0123456789abcdef"); err != nil {
			t.Fatalf("release %d: %v", i, err)
		}
	}
	after, err := s.GetPayload(ctx, object.ObjectID)
	if err != nil || after.ReaderCount != 1 {
		t.Fatalf("reader count = %d err=%v, want exactly one remaining lease", after.ReaderCount, err)
	}
}

// Expiry closes reads immediately and cleanup waits for the reader to close.
func TestPayloadExpiryClosesReadsAndCleanupRespectsReaders(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	now := time.Now().UTC()
	reservation := payloadReservation(1, 10)
	reservation.ExpiresAt = now.Add(2 * time.Second)
	admission, err := s.ReservePayload(ctx, reservation, payloadLimits())
	if err != nil || admission.Reason != "" {
		t.Fatalf("admission = %+v err=%v", admission, err)
	}
	if _, err := s.ConfirmPayloadReady(ctx, admission.Object.ObjectID, 10, reservation.Digest, now); err != nil {
		t.Fatal(err)
	}
	scope := store.PayloadReadScope{ObjectID: admission.Object.ObjectID}
	if _, err := s.OpenPayloadRead(ctx, scope, "0123456789abcdef0123456789abcdef", now); err != nil {
		t.Fatal(err)
	}
	if _, err := s.ExpirePayloads(ctx, now.Add(3*time.Second), 10); err != nil {
		t.Fatal(err)
	}
	if _, err := s.OpenPayloadRead(ctx, scope, "fedcba9876543210fedcba9876543210", now.Add(3*time.Second)); err == nil {
		t.Fatal("expired object was still readable")
	}
	claimed, err := s.ClaimPayloadCleanup(ctx, now.Add(4*time.Second), 10, time.Minute)
	if err != nil {
		t.Fatal(err)
	}
	if len(claimed) != 0 {
		t.Fatalf("cleanup claimed an object with an active reader: %+v", claimed)
	}
	if err := s.ReleasePayloadRead(ctx, admission.Object.ObjectID, "0123456789abcdef0123456789abcdef"); err != nil {
		t.Fatal(err)
	}
	claimed, err = s.ClaimPayloadCleanup(ctx, now.Add(4*time.Second), 10, time.Minute)
	if err != nil || len(claimed) != 1 {
		t.Fatalf("cleanup after reader close = %+v err=%v", claimed, err)
	}
	deleted, err := s.MarkPayloadDeleted(ctx, admission.Object.ObjectID, now.Add(4*time.Second))
	if err != nil || !deleted {
		t.Fatalf("delete = %v err=%v", deleted, err)
	}
	stats, err := s.PayloadStats(ctx, now.Add(4*time.Second))
	if err != nil {
		t.Fatal(err)
	}
	if stats.ChargedBytes != 0 {
		t.Fatalf("failed cleanup/delete kept charged bytes: %+v", stats)
	}
}

// Cancelling twice stays a success and closes admission immediately.
func TestPayloadCancelIsIdempotent(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	now := time.Now().UTC()
	admission, err := s.ReservePayload(ctx, payloadReservation(1, 10), payloadLimits())
	if err != nil || admission.Reason != "" {
		t.Fatalf("admission = %+v err=%v", admission, err)
	}
	scope := store.PayloadReadScope{ObjectID: admission.Object.ObjectID}
	for i := 0; i < 2; i++ {
		object, err := s.CancelPayload(ctx, scope, now)
		if err != nil || object.State != store.PayloadExpired {
			t.Fatalf("cancel %d = %+v err=%v", i, object, err)
		}
	}
	if _, err := s.ConfirmPayloadReady(ctx, admission.Object.ObjectID, 10, payloadDigest(1), now); err == nil {
		t.Fatal("a cancelled object became ready")
	}
}

// A foreign scope is refused without revealing whether the object exists.
func TestPayloadReadRejectsForeignScope(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	now := time.Now().UTC()
	admission, err := s.ReservePayload(ctx, payloadReservation(1, 10), payloadLimits())
	if err != nil || admission.Reason != "" {
		t.Fatalf("admission = %+v err=%v", admission, err)
	}
	if _, err := s.ConfirmPayloadReady(ctx, admission.Object.ObjectID, 10, payloadDigest(1), now); err != nil {
		t.Fatal(err)
	}
	_, err = s.OpenPayloadRead(ctx, store.PayloadReadScope{
		ObjectID: admission.Object.ObjectID,
		HostID:   "foreign-host",
	}, "0123456789abcdef0123456789abcdef", now)
	if err != store.ErrPayloadScopeMismatch {
		t.Fatalf("foreign scope = %v", err)
	}
}

// Reconciliation pages surface stale reservations and ready rows without
// touching live reservations.
func TestPayloadReconciliationQueriesBoundTheirPages(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	now := time.Now().UTC()
	admission, err := s.ReservePayload(ctx, payloadReservation(1, 10), payloadLimits())
	if err != nil || admission.Reason != "" {
		t.Fatalf("admission = %+v err=%v", admission, err)
	}
	stale, err := s.StaleReservedPayloads(ctx, now.Add(time.Minute), 10)
	if err != nil || len(stale) != 1 {
		t.Fatalf("stale = %+v err=%v", stale, err)
	}
	page, err := s.ReadyPayloadPage(ctx, "", 10)
	if err != nil || len(page) != 0 {
		t.Fatalf("ready page = %+v err=%v", page, err)
	}
}
