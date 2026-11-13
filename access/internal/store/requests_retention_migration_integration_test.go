//go:build integration

package store_test

import (
	"context"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
	"github.com/whalerein/whalerein/access/internal/store/storetest"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// Retention distinguishes new empty bodies from legacy rows with unknown history.
func TestRequestBodyRetentionHandlesLegacyAndNewEmptyBodies(t *testing.T) {
	for _, tc := range []struct {
		name                             string
		legacy, empty, cleared, wantBody bool
	}{
		{"new empty request", false, true, false, true},
		{"legacy empty request", true, true, false, false},
		{"legacy nonempty request", true, false, false, true},
		{"explicitly cleared request", false, false, true, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			s, _ := storetest.New(t)
			ctx := context.Background()
			now := time.Now().UTC()
			seedDevice(t, s, "host", "device")
			row := newRequest("request", "host", "device", "command", []byte{1}, whalereinv1.OnlineStatus_received, store.ForwardPending, now)
			if tc.empty {
				row.RequestBody = []byte{}
			}
			if _, err := s.CreateRequest(ctx, row); err != nil {
				t.Fatal(err)
			}
			if tc.legacy {
				if _, err := s.Pool().Exec(ctx, `UPDATE requests SET bodies_cleared=NULL WHERE request_id=$1`, row.RequestID); err != nil {
					t.Fatal(err)
				}
			}
			if tc.cleared {
				if _, err := s.Pool().Exec(ctx, `UPDATE requests SET request_body=''::bytea,bodies_cleared=true WHERE request_id=$1`, row.RequestID); err != nil {
					t.Fatal(err)
				}
			}
			applied, err := s.ApplyHostResult(ctx, store.HostResultUpdate{RequestID: row.RequestID, HostID: "host", DeviceID: "device", Status: whalereinv1.OnlineStatus_succeeded, ResultBody: []byte{1}, At: now})
			if err != nil || !applied {
				t.Fatal("terminal status did not advance")
			}
			result, err := s.GetRequest(ctx, row.RequestID)
			if err != nil || result.Status != whalereinv1.OnlineStatus_succeeded || (len(result.ResultBody) > 0) != tc.wantBody {
				t.Fatal("body retention did not respect its known history")
			}
		})
	}
}

// Cleanup marks already-empty expired rows so a clock rollback cannot refill them.
func TestCleanupMarksAlreadyEmptyRequestBeforeClockRollback(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	now := time.Now().UTC()
	seedDevice(t, s, "host", "device")
	row := newRequest("empty-request", "host", "device", "command", []byte{1}, whalereinv1.OnlineStatus_received, store.ForwardPending, now)
	row.RequestBody = []byte{}
	if _, err := s.CreateRequest(ctx, row); err != nil {
		t.Fatal(err)
	}
	if _, err := s.ClearExpiredBodies(ctx, row.ExpiresAt.Add(time.Second)); err != nil {
		t.Fatal(err)
	}
	var cleared bool
	if err := s.Pool().QueryRow(ctx, `SELECT bodies_cleared FROM requests WHERE request_id=$1`, row.RequestID).Scan(&cleared); err != nil || !cleared {
		t.Fatal("cleanup did not mark an already-empty row")
	}
	applied, err := s.ApplyHostResult(ctx, store.HostResultUpdate{RequestID: row.RequestID, HostID: "host", DeviceID: "device", Status: whalereinv1.OnlineStatus_succeeded, ResultBody: []byte{1}, At: now})
	if err != nil || !applied {
		t.Fatal("terminal status did not advance")
	}
	stored, err := s.GetRequest(ctx, row.RequestID)
	if err != nil || len(stored.ResultBody) != 0 {
		t.Fatal("rollback restored a cleared result")
	}
}
