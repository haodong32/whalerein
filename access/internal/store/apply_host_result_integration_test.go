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

// TestApplyHostResultRejectsBackwardTransitionsFromHostAccepted exercises the
// real PostgreSQL predicate: once a Host accepted a command the stored status
// must never move back to rejected or host_offline; it may still settle as
// succeeded.
func TestApplyHostResultRejectsBackwardTransitionsFromHostAccepted(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	now := time.Now().UTC()
	if _, err := s.Pool().Exec(ctx, `INSERT INTO hosts(host_id) VALUES ('result-host')`); err != nil {
		t.Fatal(err)
	}
	if _, err := s.Pool().Exec(ctx, `INSERT INTO devices(device_id, public_key_fingerprint) VALUES ('result-device','')`); err != nil {
		t.Fatal(err)
	}
	if _, err := s.Pool().Exec(ctx, `
		INSERT INTO requests(request_id, host_id, device_id, kind, status, expires_at)
		VALUES ('result-request','result-host','result-device',5,'received',$1)`,
		now.Add(time.Hour)); err != nil {
		t.Fatal(err)
	}
	update := func(status whalereinv1.OnlineStatus) (bool, error) {
		return s.ApplyHostResult(ctx, store.HostResultUpdate{
			RequestID: "result-request", HostID: "result-host", DeviceID: "result-device",
			Status: status, At: now,
		})
	}

	if applied, err := update(whalereinv1.OnlineStatus_host_accepted); err != nil || !applied {
		t.Fatalf("received -> host_accepted applied=%v err=%v", applied, err)
	}
	for _, forbidden := range []whalereinv1.OnlineStatus{
		whalereinv1.OnlineStatus_rejected,
		whalereinv1.OnlineStatus_host_offline,
	} {
		applied, err := update(forbidden)
		if err != nil || applied {
			t.Fatalf("host_accepted -> %s applied=%v err=%v", forbidden, applied, err)
		}
		row, err := s.GetRequest(ctx, "result-request")
		if err != nil {
			t.Fatal(err)
		}
		if row.Status != whalereinv1.OnlineStatus_host_accepted {
			t.Fatalf("status moved backward to %s", row.Status)
		}
	}
	if applied, err := update(whalereinv1.OnlineStatus_succeeded); err != nil || !applied {
		t.Fatalf("host_accepted -> succeeded applied=%v err=%v", applied, err)
	}
	row, err := s.GetRequest(ctx, "result-request")
	if err != nil || row.Status != whalereinv1.OnlineStatus_succeeded {
		t.Fatalf("final status=%v err=%v", row.Status, err)
	}
}
