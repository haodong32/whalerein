package store

import (
	"context"
	"errors"
	"slices"
	"testing"
	"time"

	"github.com/jackc/pgx/v5/pgconn"
)

func TestRevocationScopeFreezesGenerationsAndConnections(t *testing.T) {
	connection := ScopeConnection{Ref: ConnectionRef{Epoch: "epoch-a", ID: 7}, ClientID: "whalerein-device-d1"}
	scope := RevocationScope{
		HostID: "h1", DeviceID: "d1",
		EnrollmentGeneration: 3,
		Grants: []ScopeGrant{{
			HostID: "h1", DeviceID: "d1",
			EnrollmentGeneration: 3, GrantGeneration: 2,
			RevocationGeneration: 4, DeviceRevocationGeneration: 5,
			Connections: []ScopeConnection{connection},
		}},
	}
	frozen := scope.Frozen()
	scope.Grants[0].GrantGeneration = 99
	scope.Grants[0].Connections[0] = ScopeConnection{}
	if frozen.Grants[0].GrantGeneration != 2 {
		t.Fatal("frozen grant generation aliased the caller")
	}
	if frozen.Grants[0].Connections[0] != connection {
		t.Fatal("frozen connection aliased the caller")
	}
	if want := uint64(5)<<40 | uint64(4)<<20 | uint64(2); frozen.Grants[0].Revision() != want {
		t.Fatalf("revision = %d, want %d", frozen.Grants[0].Revision(), want)
	}
	if !frozen.EnrollmentReplaced(4) || frozen.EnrollmentReplaced(3) {
		t.Fatal("enrollment replacement comparison")
	}
}

func TestCloudDeviceRevocationScopeUsesFrozenGenerations(t *testing.T) {
	receipt := CloudDeviceRevocation{
		HostID: "h1", DeviceID: "d1", EnrollmentGeneration: 6,
		GrantGeneration: 3, RevocationGeneration: 2, DeviceRevocationGeneration: 4,
		Exists: true,
	}
	scope := receipt.Scope()
	if scope.EnrollmentGeneration != 6 || len(scope.Grants) != 1 {
		t.Fatalf("scope = %+v", scope)
	}
	if scope.Grants[0].RevocationGeneration != 3 {
		t.Fatalf("revocation generation = %d", scope.Grants[0].RevocationGeneration)
	}
	if scope.Grants[0].Revision() != (4<<40 | 3<<20 | 3) {
		t.Fatalf("fence revision = %d", scope.Grants[0].Revision())
	}
}

func TestCompleteCloudDeviceRevocationAtRecordsAppliedConnection(t *testing.T) {
	now := time.Now().UTC()
	reference := &ConnectionRef{Epoch: "epoch-a", ID: 17}
	var got []any
	database := &fakeDB{t: t, execFn: func(_ context.Context, statement string, args ...any) (pgconn.CommandTag, error) {
		if statement == "" || len(args) != 6 {
			t.Fatalf("unexpected write: %q %v", statement, args)
		}
		got = append([]any(nil), args...)
		return updateTag(1), nil
	}}
	if err := newStore(database).CompleteCloudDeviceRevocationAt(context.Background(), "h1", "d1", 6, reference, now); err != nil {
		t.Fatal(err)
	}
	if !slices.Equal(got, []any{"h1", "d1", int64(6), now, "epoch-a", int64(17)}) {
		t.Fatalf("applied connection was not recorded: %v", got)
	}

	missing := &fakeDB{t: t, execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
		return updateTag(0), nil
	}}
	if err := newStore(missing).CompleteCloudDeviceRevocationAt(context.Background(), "h1", "d1", 6, nil, now); !errors.Is(err, ErrNotFound) {
		t.Fatalf("missing receipt error = %v", err)
	}
}

func TestMarkRemovalEnforcedAtRecordsAppliedConnection(t *testing.T) {
	now := time.Now().UTC()
	reference := &ConnectionRef{Epoch: "epoch-b", ID: 21}
	var got []any
	database := &fakeDB{t: t, execFn: func(_ context.Context, statement string, args ...any) (pgconn.CommandTag, error) {
		if len(args) != 4 {
			t.Fatalf("unexpected write: %q %v", statement, args)
		}
		got = append([]any(nil), args...)
		return updateTag(1), nil
	}}
	if err := newStore(database).MarkRemovalEnforcedAt(context.Background(), "rem-1", reference, now); err != nil {
		t.Fatal(err)
	}
	if !slices.Equal(got, []any{"rem-1", now, "epoch-b", int64(21)}) {
		t.Fatalf("applied removal connection = %v", got)
	}
	missing := &fakeDB{t: t, execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
		return updateTag(0), nil
	}}
	if err := newStore(missing).MarkRemovalEnforcedAt(context.Background(), "rem-1", nil, now); !errors.Is(err, ErrNotFound) {
		t.Fatalf("missing removal error = %v", err)
	}
}

func TestParseConnectionRefRoundTrip(t *testing.T) {
	reference, ok := ParseConnectionRef("epoch-a:17")
	if !ok || reference.String() != "epoch-a:17" || !reference.Valid() {
		t.Fatalf("reference = %+v ok=%v", reference, ok)
	}
	for _, value := range []string{"", "epoch-a", ":1", "epoch-a:0", "epoch-a:x", "ep och:1"} {
		if _, ok := ParseConnectionRef(value); ok {
			t.Fatalf("accepted invalid reference %q", value)
		}
	}
}
