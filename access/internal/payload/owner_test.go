//go:build unix

package payload

import (
	"bytes"
	"context"
	"errors"
	"io"
	"os"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
)

func ownedConfig(root, owner string) Config {
	config := testConfig(root)
	config.NodeID = owner
	config.OwnerID = owner
	return config
}

// A fetch on a node that does not own the object must be a pure miss: no
// lease, no cancellation and no change to the owning node's row. Repeated
// probes stay harmless and the owner can still read the object.
func TestWrongNodeFetchDoesNotMutateTheOwnerRow(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	body := []byte("owned-ciphertext")
	object := objectFixture(now)
	object.OwnerID = "node-a"
	object.State = store.PayloadReady
	object.Length, object.Reserved, object.Digest = int64(len(body)), int64(len(body)), digestOf(body)

	opens, releases, cancels := 0, 0, 0
	db := &fakeStore{t: t}
	db.getFn = func(context.Context, string) (store.PayloadObject, error) { return object, nil }
	db.openFn = func(_ context.Context, scope store.PayloadReadScope, _ string, _ time.Time) (store.PayloadObject, error) {
		if scope.OwnerID != "node-a" {
			t.Fatalf("lease opened for owner %q", scope.OwnerID)
		}
		opens++
		return object, nil
	}
	db.releaseFn = func(context.Context, string, string) error {
		releases++
		return nil
	}
	db.cancelFn = func(context.Context, store.PayloadReadScope, time.Time) (store.PayloadObject, error) {
		cancels++
		return object, nil
	}

	ownerA := newTestService(t, db, ownedConfig(t.TempDir(), "node-a"), now)
	mustWriteFile(t, ownerA.finalPath(object.ObjectID), body)
	nodeB := newTestService(t, db, ownedConfig(t.TempDir(), "node-b"), now)

	for probe := 0; probe < 3; probe++ {
		_, _, err := nodeB.Open(context.Background(), Ref{ObjectID: object.ObjectID})
		requireError(t, err, ErrNotFound)
	}
	if opens != 0 || cancels != 0 {
		t.Fatalf("wrong-node probe opened=%d cancelled=%d, want no mutation", opens, cancels)
	}
	// A wrong-node delete is also a non-destructive miss.
	requireError(t, nodeB.Cancel(context.Background(), Ref{ObjectID: object.ObjectID}), ErrNotFound)
	if cancels != 0 {
		t.Fatalf("wrong-node cancel mutated the row: %d", cancels)
	}

	reader, got, err := ownerA.Open(context.Background(), Ref{ObjectID: object.ObjectID})
	if err != nil {
		t.Fatalf("owner Open: %v", err)
	}
	data, err := io.ReadAll(reader)
	if err != nil || !bytes.Equal(data, body) || got.OwnerID != "node-a" {
		t.Fatalf("owner read = %q object=%+v err=%v", data, got, err)
	}
	if err := reader.Close(); err != nil {
		t.Fatal(err)
	}
	if opens != 1 || releases != 1 || cancels != 0 {
		t.Fatalf("owner opens=%d releases=%d cancels=%d", opens, releases, cancels)
	}
}

// Expiry and finish paths on a non-owner node must not close the owner's
// reservation; only the owning node may fence its own row.
func TestWrongNodeWriteAndFinishDoNotCloseOwnerReservation(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	expired := objectFixture(now)
	expired.OwnerID = "node-a"
	expired.State = store.PayloadReserved
	expired.ExpiresAt = now.Add(-time.Second)
	cancels := 0
	db := &fakeStore{t: t}
	db.getFn = func(context.Context, string) (store.PayloadObject, error) { return expired, nil }
	db.cancelFn = func(context.Context, store.PayloadReadScope, time.Time) (store.PayloadObject, error) {
		cancels++
		return expired, nil
	}

	nodeB := newTestService(t, db, ownedConfig(t.TempDir(), "node-b"), now)
	requireError(t, nodeB.Write(context.Background(), Ref{ObjectID: expired.ObjectID}, bytes.NewReader([]byte("body"))), ErrNotFound)
	if _, err := nodeB.Finish(context.Background(), Ref{ObjectID: expired.ObjectID}); !errors.Is(err, ErrNotFound) {
		t.Fatalf("wrong-node finish = %v", err)
	}
	if cancels != 0 {
		t.Fatalf("wrong-node write/finish cancelled owner rows: %d", cancels)
	}

	ownerA := newTestService(t, db, ownedConfig(t.TempDir(), "node-a"), now)
	requireError(t, ownerA.Write(context.Background(), Ref{ObjectID: expired.ObjectID}, bytes.NewReader([]byte("body"))), ErrExpired)
	if cancels != 1 {
		t.Fatalf("owner expiry did not close its own reservation: cancels=%d", cancels)
	}
}

// Recovery and cleanup only touch rows and files whose metadata owner is this
// node. A distinct-root peer's ready rows and files stay untouched.
func TestRecoveryAndCleanupAreOwnerScoped(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	config := ownedConfig(t.TempDir(), "node-a")
	foreign := objectFixture(now)
	foreign.ObjectID = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
	foreign.OwnerID = "node-b"
	foreign.State = store.PayloadReady
	ownedTombstone := objectFixture(now)
	ownedTombstone.ObjectID = "cccccccccccccccccccccccccccccccc"
	ownedTombstone.OwnerID = "node-a"
	ownedTombstone.State = store.PayloadDeleted

	deleted := map[string]int{}
	db := &fakeStore{t: t}
	db.expireFn = func(context.Context, time.Time, int) ([]store.PayloadObject, error) { return nil, nil }
	db.staleFn = func(context.Context, time.Time, int) ([]store.PayloadObject, error) { return nil, nil }
	db.readyPageFn = func(context.Context, string, int) ([]store.PayloadObject, error) { return nil, nil }
	db.tombstoneFn = func(context.Context, time.Time, int) (int64, error) { return 0, nil }
	db.getFn = func(_ context.Context, id string) (store.PayloadObject, error) {
		switch id {
		case foreign.ObjectID:
			return foreign, nil
		case ownedTombstone.ObjectID:
			return ownedTombstone, nil
		default:
			return store.PayloadObject{}, errors.New("unexpected object")
		}
	}
	service := newTestService(t, db, config, now)
	mustWriteFile(t, service.finalPath(foreign.ObjectID), []byte("foreign"))
	mustWriteFile(t, service.finalPath(ownedTombstone.ObjectID), []byte("owned"))
	if err := service.Recover(context.Background()); err != nil {
		t.Fatalf("Recover: %v", err)
	}
	if db.expireOwner != "node-a" || db.staleOwner != "node-a" || db.readyOwner != "node-a" {
		t.Fatalf("recovery owners = %q/%q/%q", db.expireOwner, db.staleOwner, db.readyOwner)
	}
	if _, err := os.Stat(service.finalPath(foreign.ObjectID)); err != nil {
		t.Fatalf("recovery removed a foreign owner's file: %v", err)
	}
	if _, err := os.Stat(service.finalPath(ownedTombstone.ObjectID)); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("recovery kept an owned deleted file: %v", err)
	}

	claimedForeign := objectFixture(now)
	claimedForeign.ObjectID = "dddddddddddddddddddddddddddddddd"
	claimedForeign.OwnerID = "node-b"
	claimedForeign.State = store.PayloadExpired
	claimedOwned := objectFixture(now)
	claimedOwned.ObjectID = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"
	claimedOwned.OwnerID = "node-a"
	claimedOwned.State = store.PayloadExpired
	db.claimFn = func(context.Context, time.Time, int, time.Duration) ([]store.PayloadObject, error) {
		return []store.PayloadObject{claimedForeign, claimedOwned}, nil
	}
	db.deletedFn = func(_ context.Context, id string, _ time.Time) (bool, error) {
		deleted[id]++
		return true, nil
	}
	mustWriteFile(t, service.finalPath(claimedForeign.ObjectID), []byte("foreign-expired"))
	mustWriteFile(t, service.finalPath(claimedOwned.ObjectID), []byte("owned-expired"))
	result, err := service.Cleanup(context.Background())
	if err != nil {
		t.Fatalf("Cleanup: %v", err)
	}
	if db.claimOwner != "node-a" || db.tombstoneOwner != "node-a" {
		t.Fatalf("cleanup owners = %q/%q", db.claimOwner, db.tombstoneOwner)
	}
	if _, err := os.Stat(service.finalPath(claimedForeign.ObjectID)); err != nil {
		t.Fatalf("cleanup removed a foreign owner's expired file: %v", err)
	}
	if _, err := os.Stat(service.finalPath(claimedOwned.ObjectID)); !errors.Is(err, os.ErrNotExist) {
		t.Fatal("cleanup kept an owned expired file")
	}
	if deleted[claimedForeign.ObjectID] != 0 || deleted[claimedOwned.ObjectID] != 1 {
		t.Fatalf("deleted rows = %v", deleted)
	}
	if result.Deleted != 1 {
		t.Fatalf("result = %+v", result)
	}

	if _, err := service.Stats(context.Background()); err != nil {
		t.Fatal(err)
	}
	if db.statsOwner != "node-a" {
		t.Fatalf("stats owner = %q", db.statsOwner)
	}
}
