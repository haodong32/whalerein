package store

import (
	"context"
	"errors"
	"math"
	"testing"

	"github.com/jackc/pgx/v5"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// releaseRow fills every single-snapshot release target plus the seven
// authority booleans the release statement computes.
func releaseRow() pgx.Row {
	return syntheticRow{fill: func(dest ...any) {
		if len(dest) != 38 {
			panic("unexpected release scan width")
		}
		// Reuse the shared fixture filler for the request columns; the
		// authority booleans default to false (every fence denied).
		requestFixtureRow([]byte{1}).Scan(dest[:31]...)
		for index := 31; index < 38; index++ {
			*dest[index].(*bool) = false
		}
	}}
}

// A release without an exact device/request identity is a miss, not a
// database lookup that could disclose another device's row.
func TestReleaseRequestRejectsIncompleteScopeWithoutQuery(t *testing.T) {
	store := newStore(&fakeDB{t: t})
	for _, scope := range []ReleaseScope{
		{RequestID: "request"},
		{DeviceID: "device"},
		{},
	} {
		if _, err := store.ReleaseRequest(context.Background(), scope); !errors.Is(err, ErrNotFound) {
			t.Fatalf("scope %+v = %v", scope, err)
		}
	}
}

// A negative delivery allowance must be clamped to zero instead of widening
// the response bound or reaching PostgreSQL as a negative.
func TestReleaseRequestClampsNegativeDeliveryLimit(t *testing.T) {
	var seen []any
	db := &fakeDB{t: t, queryRowFn: func(_ context.Context, _ string, args ...any) pgx.Row {
		seen = args
		return releaseRow()
	}}
	outcome, err := newStore(db).ReleaseRequest(context.Background(), ReleaseScope{
		DeviceID: "device", RequestID: "request", DeliveryLimitBytes: -4096,
	})
	if err != nil {
		t.Fatal(err)
	}
	if len(seen) != 5 {
		t.Fatalf("release args = %v", seen)
	}
	if seen[0] != "request" || seen[1] != "device" || seen[3] != int64(0) {
		t.Fatalf("release bound not clamped: %v", seen)
	}
	if outcome.WithinDelivery {
		t.Fatal("empty row reported within a clamped delivery bound")
	}
}

// Missing rows and storage failures stay distinguishable so the HTTP layer can
// report a fence rather than a generic server error.
func TestReleaseRequestSeparatesMissingRowsFromStorageFailures(t *testing.T) {
	missing := newStore(&fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row {
		return syntheticRow{err: pgx.ErrNoRows}
	}})
	if _, err := missing.ReleaseRequest(context.Background(), ReleaseScope{DeviceID: "device", RequestID: "request"}); !errors.Is(err, ErrNotFound) {
		t.Fatalf("missing row = %v", err)
	}
	broken := newStore(&fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row {
		return syntheticRow{err: errStoreUnit}
	}})
	if _, err := broken.ReleaseRequest(context.Background(), ReleaseScope{DeviceID: "device", RequestID: "request"}); !errors.Is(err, errStoreUnit) {
		t.Fatalf("storage failure = %v", err)
	}
}

// Newly admitted encrypted requests must carry the full durable grant stamp.
// Partial or out-of-range stamps fail closed before any insert.
func TestCreateRequestRequiresACompleteGrantStamp(t *testing.T) {
	query := func(context.Context, string, ...any) pgx.Row {
		t.Fatal("invalid stamp reached the database")
		return nil
	}
	base := Request{
		RequestID: "request", HostID: "host", DeviceID: "device", CommandID: "command",
		Status: whalereinv1.OnlineStatus_received, ForwardState: ForwardNone,
	}
	for name, tc := range map[string]struct {
		kind  whalereinv1.RequestKind
		stamp *GrantStamp
	}{
		"query without stamp":         {whalereinv1.RequestKind_REQUEST_KIND_QUERY, nil},
		"command without stamp":       {whalereinv1.RequestKind_REQUEST_KIND_COMMAND, nil},
		"negative result limit":       {whalereinv1.RequestKind_REQUEST_KIND_COMMAND, &GrantStamp{ResultLimitBytes: -1}},
		"grant revision out of range": {whalereinv1.RequestKind_REQUEST_KIND_COMMAND, &GrantStamp{GrantGeneration: math.MaxInt64 + 1}},
		"revocation out of range":     {whalereinv1.RequestKind_REQUEST_KIND_COMMAND, &GrantStamp{RevocationGeneration: math.MaxInt64 + 1}},
		"device revision out of range": {whalereinv1.RequestKind_REQUEST_KIND_COMMAND,
			&GrantStamp{DeviceRevocationGeneration: math.MaxInt64 + 1}},
	} {
		t.Run(name, func(t *testing.T) {
			req := base
			req.Kind = tc.kind
			req.Stamp = tc.stamp
			if _, err := createRequest(context.Background(), query, req); !errors.Is(err, ErrMissingGrantStamp) {
				t.Fatalf("err = %v", err)
			}
		})
	}

	// A handshake keeps its historical no-stamp shape and still reaches the
	// insert path.
	req := base
	req.Kind = whalereinv1.RequestKind_REQUEST_KIND_HANDSHAKE
	req.CommandID = ""
	calls := 0
	insert := func(_ context.Context, _ string, _ ...any) pgx.Row {
		calls++
		return requestFixtureRow([]byte{1})
	}
	if outcome, err := createRequest(context.Background(), insert, req); err != nil || !outcome.Created || calls != 1 {
		t.Fatalf("handshake without stamp = %+v/%v calls=%d", outcome, err, calls)
	}
}

func TestCreateRequestRejectsUnknownClientLeaseState(t *testing.T) {
	req := Request{
		RequestID: "request", HostID: "host", DeviceID: "device", CommandID: "command",
		Kind: whalereinv1.RequestKind_REQUEST_KIND_HANDSHAKE, Status: whalereinv1.OnlineStatus_received,
		ClientLeaseState: ClientLeaseState("unknown-state"),
	}
	if _, err := createRequest(context.Background(), func(context.Context, string, ...any) pgx.Row {
		t.Fatal("invalid lease state reached the database")
		return nil
	}, req); err == nil {
		t.Fatal("unknown client lease state accepted")
	}
}
