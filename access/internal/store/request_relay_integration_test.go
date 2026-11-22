//go:build integration

package store_test

import (
	"bytes"
	"context"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
	"github.com/whalerein/whalerein/access/internal/store/storetest"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

func relayRequestFixture(t *testing.T, s *store.Store, requestID string, now time.Time) store.Request {
	t.Helper()
	return store.Request{
		RequestID:        requestID,
		HostID:           "host",
		DeviceID:         "device",
		CommandID:        "command-" + requestID,
		Kind:             whalereinv1.RequestKind_REQUEST_KIND_COMMAND,
		ProtocolVersion:  1,
		RelayVersion:     4,
		Status:           whalereinv1.OnlineStatus_received,
		PayloadDigest:    []byte("digest"),
		BodyReference:    []byte("serialized-reference"),
		ChannelID:        "channel",
		ExpiresAt:        now.Add(time.Hour),
		ForwardState:     store.ForwardNone,
		ClientLeaseUntil: &[]time.Time{now.Add(10 * time.Second)}[0],
		ClientLeaseState: store.ClientLeaseLive,
		Stamp:            currentStamp(t, s, "host", "device", 1<<20),
	}
}

// TestRequestReferenceAndNoticeIntentRoundTrip stores an epoch-4 body and
// result reference and records one durable, coalesced notice intent.
func TestRequestReferenceAndNoticeIntentRoundTrip(t *testing.T) {
	s, _ := storetest.New(t)
	seedDevice(t, s, "host", "device")
	// PostgreSQL keeps microseconds; truncate so the exact stored deadline is
	// comparable with a value read back from the row.
	now := time.Now().UTC().Truncate(time.Microsecond)
	row := relayRequestFixture(t, s, "r1", now)
	outcome, err := s.CreateRequest(context.Background(), row)
	if err != nil || !outcome.Created {
		t.Fatalf("create: %+v err=%v", outcome, err)
	}
	loaded, err := s.GetRequest(context.Background(), "r1")
	if err != nil {
		t.Fatal(err)
	}
	if loaded.RelayVersion != 4 || !bytes.Equal(loaded.BodyReference, row.BodyReference) {
		t.Fatalf("reference representation lost: %+v", loaded)
	}

	encodedResult := []byte("serialized-result-reference")
	applied, err := s.ApplyHostResult(context.Background(), store.HostResultUpdate{
		RequestID: "r1", HostID: "host", DeviceID: "device",
		Status: whalereinv1.OnlineStatus_succeeded, ErrorCode: whalereinv1.ErrorCode_internal,
		ResultReference: encodedResult, At: now,
	})
	if err != nil || !applied {
		t.Fatalf("apply: applied=%v err=%v", applied, err)
	}
	loaded, err = s.GetRequest(context.Background(), "r1")
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(loaded.ResultReference, encodedResult) || loaded.Status != whalereinv1.OnlineStatus_succeeded ||
		loaded.ErrorCode != whalereinv1.ErrorCode_internal {
		t.Fatalf("outcome/reference lost: %+v", loaded)
	}

	claims, err := s.ClaimNoticeIntents(context.Background(), now, 10, time.Minute)
	if err != nil || len(claims) != 1 {
		t.Fatalf("claims=%d err=%v", len(claims), err)
	}
	if claims[0].NoticeStatus != whalereinv1.OnlineStatus_succeeded || !claims[0].NoticePending {
		t.Fatalf("notice intent = %+v", claims[0])
	}
	published, err := s.MarkNoticePublished(context.Background(), "r1", whalereinv1.OnlineStatus_succeeded, now, now)
	if err != nil || !published {
		t.Fatalf("mark published = %v err=%v", published, err)
	}
	backlog, err := s.NoticeStats(context.Background())
	if err != nil || backlog.Pending != 0 {
		t.Fatalf("backlog = %+v err=%v", backlog, err)
	}
}

// TestNoticeIntentCoalescesIntermediateStateAndKeepsTerminal keeps only the
// newest state pending and refuses to mark an older status published.
func TestNoticeIntentCoalescesIntermediateStateAndKeepsTerminal(t *testing.T) {
	s, _ := storetest.New(t)
	seedDevice(t, s, "host", "device")
	// PostgreSQL keeps microseconds; truncate so the exact stored deadline is
	// comparable with a value read back from the row.
	now := time.Now().UTC().Truncate(time.Microsecond)
	row := relayRequestFixture(t, s, "r1", now)
	if _, err := s.CreateRequest(context.Background(), row); err != nil {
		t.Fatal(err)
	}
	for _, status := range []whalereinv1.OnlineStatus{
		whalereinv1.OnlineStatus_host_accepted,
		whalereinv1.OnlineStatus_succeeded,
	} {
		applied, err := s.ApplyHostResult(context.Background(), store.HostResultUpdate{
			RequestID: "r1", HostID: "host", DeviceID: "device", Status: status, At: now,
		})
		if err != nil || !applied {
			t.Fatalf("apply %s: %v %v", status, applied, err)
		}
	}
	claims, err := s.ClaimNoticeIntents(context.Background(), now, 10, time.Minute)
	if err != nil || len(claims) != 1 {
		t.Fatalf("claims=%d err=%v", len(claims), err)
	}
	if claims[0].NoticeStatus != whalereinv1.OnlineStatus_succeeded {
		t.Fatalf("intermediate state not coalesced: %+v", claims[0])
	}
	// A concurrent newer state leaves the intent pending instead of recording
	// the older publication.
	published, err := s.MarkNoticePublished(context.Background(), "r1", whalereinv1.OnlineStatus_host_accepted, now, now)
	if err != nil || published {
		t.Fatalf("stale status marked published: %v %v", published, err)
	}
	published, err = s.MarkNoticePublished(context.Background(), "r1", whalereinv1.OnlineStatus_succeeded, now, now)
	if err != nil || !published {
		t.Fatalf("terminal notice not published: %v %v", published, err)
	}
}

// TestHeartbeatRenewsOnlyExactLiveRequestAndNeverExtendsDeadline renews only
// the exact live Device request and never extends its hard deadline.
func TestHeartbeatRenewsOnlyExactLiveRequestAndNeverExtendsDeadline(t *testing.T) {
	s, _ := storetest.New(t)
	seedDevice(t, s, "host", "device")
	// PostgreSQL keeps microseconds; truncate so the exact stored deadline is
	// comparable with a value read back from the row.
	now := time.Now().UTC().Truncate(time.Microsecond)
	row := relayRequestFixture(t, s, "r1", now)
	if _, err := s.CreateRequest(context.Background(), row); err != nil {
		t.Fatal(err)
	}
	originalDeadline := row.ExpiresAt
	outcome, err := s.HeartbeatRequest(context.Background(), "device", "", "r1", 10*time.Second)
	if err != nil || !outcome.Renewed {
		t.Fatalf("heartbeat = %+v err=%v", outcome, err)
	}
	if !outcome.Request.ExpiresAt.Equal(originalDeadline) {
		t.Fatal("heartbeat extended the hard deadline")
	}
	if outcome.Request.ClientLeaseUntil == nil || outcome.Request.ClientLeaseUntil.After(originalDeadline) {
		t.Fatal("lease crossed the hard deadline")
	}

	// A foreign device cannot renew and does not learn the row.
	foreign, err := s.HeartbeatRequest(context.Background(), "other", "", "r1", 10*time.Second)
	if err != nil || foreign.Renewed || foreign.Failure != store.HeartbeatDeviceForeign {
		t.Fatalf("foreign heartbeat = %+v err=%v", foreign, err)
	}

	// A terminal transition permanently ends renewal.
	if _, err := s.ApplyHostResult(context.Background(), store.HostResultUpdate{
		RequestID: "r1", HostID: "host", DeviceID: "device",
		Status: whalereinv1.OnlineStatus_succeeded, At: now,
	}); err != nil {
		t.Fatal(err)
	}
	terminal, err := s.HeartbeatRequest(context.Background(), "device", "", "r1", 10*time.Second)
	if err != nil || terminal.Renewed || terminal.Failure != store.HeartbeatTerminal {
		t.Fatalf("terminal heartbeat = %+v err=%v", terminal, err)
	}

	// An elapsed hard deadline is irreversible.
	expired := relayRequestFixture(t, s, "r2", now)
	expired.ExpiresAt = now.Add(-time.Second)
	expired.ClientLeaseUntil = &[]time.Time{now.Add(time.Minute)}[0]
	if _, err := s.CreateRequest(context.Background(), expired); err != nil {
		t.Fatal(err)
	}
	deadline, err := s.HeartbeatRequest(context.Background(), "device", "", "r2", 10*time.Second)
	if err != nil || deadline.Renewed || deadline.Failure != store.HeartbeatHardDeadline {
		t.Fatalf("deadline heartbeat = %+v err=%v", deadline, err)
	}
}

// TestPublishExhaustionRecordsNoticeIntent records a durable notice intent
// when publication retries are exhausted.
func TestPublishExhaustionRecordsNoticeIntent(t *testing.T) {
	s, _ := storetest.New(t)
	seedDevice(t, s, "host", "device")
	// PostgreSQL keeps microseconds; truncate so the exact stored deadline is
	// comparable with a value read back from the row.
	now := time.Now().UTC().Truncate(time.Microsecond)
	row := relayRequestFixture(t, s, "r1", now)
	row.CommandID = ""
	row.Kind = whalereinv1.RequestKind_REQUEST_KIND_QUERY
	row.PayloadDigest = []byte{}
	if _, err := s.CreateRequest(context.Background(), row); err != nil {
		t.Fatal(err)
	}
	if err := s.MarkPublishFailure(context.Background(), "r1", now, time.Second, 0); err != nil {
		t.Fatal(err)
	}
	loaded, err := s.GetRequest(context.Background(), "r1")
	if err != nil {
		t.Fatal(err)
	}
	if loaded.Status != whalereinv1.OnlineStatus_unknown || !loaded.NoticePending ||
		loaded.NoticeStatus != whalereinv1.OnlineStatus_unknown {
		t.Fatalf("publish exhaustion did not record a notice: %+v", loaded)
	}
}
