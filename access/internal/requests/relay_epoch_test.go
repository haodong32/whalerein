package requests

import (
	"bytes"
	"context"
	"crypto/sha256"
	"errors"
	"testing"
	"time"

	"google.golang.org/protobuf/proto"

	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

func testReference(purpose whalereinv1.PayloadPurpose, ciphertext []byte) *whalereinv1.PayloadReference {
	digest := sha256.Sum256(ciphertext)
	return &whalereinv1.PayloadReference{
		ObjectId:        "0123456789abcdef0123456789abcdef",
		OwnerNodeId:     "node-1",
		CiphertextBytes: uint64(len(ciphertext)),
		Sha256:          digest[:],
		ExpiresAtUnixMs: time.Date(2026, 11, 9, 12, 10, 0, 0, time.UTC).UnixMilli(),
		Purpose:         purpose,
	}
}

func referencedCommand(now time.Time) (*whalereinv1.HostRequest, *whalereinv1.PayloadReference) {
	ciphertext := []byte("complete sealed command")
	reference := testReference(whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_REQUEST, ciphertext)
	request := validRequest(now, "referenced-cmd")
	request.Body = nil
	request.BodyReference = reference
	request.PayloadDigest = append([]byte(nil), reference.GetSha256()...)
	return request, reference
}

func TestSubmitRejectsForeignEpochAndInvalidRepresentations(t *testing.T) {
	f := newFixture(t)

	cases := []struct {
		name    string
		mutate  func(*whalereinv1.HostRequest)
		wantErr error
	}{
		{"foreign epoch", func(r *whalereinv1.HostRequest) { r.RelayVersion = 2 }, ErrProtocolVersion},
		{"missing epoch", func(r *whalereinv1.HostRequest) { r.RelayVersion = 0 }, ErrProtocolVersion},
		{"command both representations", func(r *whalereinv1.HostRequest) {
			r.BodyReference = testReference(whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_REQUEST, r.Body)
			r.PayloadDigest = r.BodyReference.Sha256
		}, ErrInvalidRequest},
		{"command neither representation", func(r *whalereinv1.HostRequest) { r.Body = nil }, ErrInvalidRequest},
		{"query both representations", func(r *whalereinv1.HostRequest) {
			r.Kind = whalereinv1.RequestKind_REQUEST_KIND_QUERY
			r.CommandId = ""
			r.PayloadDigest = nil
			r.BodyReference = testReference(whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_REQUEST, r.Body)
			r.PayloadDigest = r.BodyReference.Sha256
		}, ErrInvalidRequest},
		{"query neither representation", func(r *whalereinv1.HostRequest) {
			r.Kind = whalereinv1.RequestKind_REQUEST_KIND_QUERY
			r.CommandId = ""
			r.PayloadDigest = nil
			r.Body = nil
		}, ErrInvalidRequest},
		{"query reference without digest", func(r *whalereinv1.HostRequest) {
			r.Kind = whalereinv1.RequestKind_REQUEST_KIND_QUERY
			r.CommandId = ""
			r.PayloadDigest = nil
			r.Body = nil
			r.BodyReference = testReference(whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_REQUEST, []byte("payload"))
		}, ErrInvalidRequest},
		{"handshake reference", func(r *whalereinv1.HostRequest) {
			r.Kind = whalereinv1.RequestKind_REQUEST_KIND_HANDSHAKE
			r.CommandId = ""
			r.PayloadDigest = nil
			r.Body = nil
			r.BodyReference = testReference(whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_REQUEST, []byte("handshake"))
		}, ErrInvalidRequest},
		{"reference digest differs from payload_digest", func(r *whalereinv1.HostRequest) {
			r.Body = nil
			r.BodyReference = testReference(whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_REQUEST, []byte("payload"))
			r.PayloadDigest = bytes.Repeat([]byte{9}, sha256.Size)
		}, ErrDigestMismatch},
		{"reference purpose differs", func(r *whalereinv1.HostRequest) {
			r.Body = nil
			r.BodyReference = testReference(whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_RESULT, []byte("payload"))
			r.PayloadDigest = r.BodyReference.Sha256
		}, ErrInvalidRequest},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			request := validRequest(f.now, "rep-cmd")
			tc.mutate(request)
			if _, _, err := f.service.Submit(context.Background(), input(request)); !errors.Is(err, tc.wantErr) {
				t.Fatalf("err = %v, want %v", err, tc.wantErr)
			}
		})
	}
	if len(f.store.rows) != 0 {
		t.Fatalf("invalid representation reached durable admission: %d rows", len(f.store.rows))
	}
}

func TestSubmitReferencedCommandPersistsAndForwardsReference(t *testing.T) {
	f := newFixture(t)
	request, reference := referencedCommand(f.now)
	row, created, err := f.service.Submit(context.Background(), input(request))
	if err != nil || !created {
		t.Fatalf("submit: created=%v err=%v", created, err)
	}
	if len(row.RequestBody) != 0 || !bytes.Equal(row.BodyReference, mustMarshal(t, reference)) {
		t.Fatal("referenced representation was not preserved durably")
	}
	if len(f.payloads.calls) != 1 || f.payloads.calls[0].scope.Purpose != whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_REQUEST {
		t.Fatalf("reference scope not checked: %+v", f.payloads.calls)
	}
	f.store.claimRows = []store.Request{row}
	if err := f.service.ForwardDue(context.Background()); err != nil {
		t.Fatal(err)
	}
	if len(f.broker.published) != 1 {
		t.Fatalf("published %d envelopes", len(f.broker.published))
	}
	envelope := f.broker.published[0]
	if envelope.GetRelayVersion() != RelayEpoch || envelope.GetBodyReference() == nil || len(envelope.GetBody()) != 0 {
		t.Fatalf("forwarded envelope = %+v", envelope)
	}
	if !bytes.Equal(envelope.GetBodyReference().GetSha256(), reference.GetSha256()) {
		t.Fatal("forwarded reference digest changed")
	}
}

func TestSubmitReferencedCommandRequiresReadableObject(t *testing.T) {
	f := newFixture(t)
	f.payloads.err = errors.New("object missing")
	request, _ := referencedCommand(f.now)
	if _, _, err := f.service.Submit(context.Background(), input(request)); !errors.Is(err, ErrUnavailable) {
		t.Fatalf("unreadable reference = %v", err)
	}
	if len(f.store.rows) != 0 {
		t.Fatal("unreadable reference was admitted")
	}
}

func TestHandleHostResultPersistsReferenceWhenObjectUnreadable(t *testing.T) {
	f := newFixture(t)
	f.store.rows["r1"] = store.Request{
		RequestID: "r1", HostID: "h1", DeviceID: "d1", CommandID: "c1",
		Kind: whalereinv1.RequestKind_REQUEST_KIND_COMMAND, Status: whalereinv1.OnlineStatus_received,
		PayloadDigest: []byte("digest"), ChannelID: "channel-1",
		Stamp: &store.GrantStamp{GrantGeneration: 7, RevocationGeneration: 3, DeviceRevocationGeneration: 2, ResultLimitBytes: 1 << 20},
	}
	ciphertext := []byte("complete sealed result")
	reference := testReference(whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_RESULT, ciphertext)
	f.payloads.err = errors.New("object missing")
	result := &whalereinv1.HostResult{
		RequestId: "r1", HostId: "h1", DeviceId: "d1", CommandId: "c1",
		Status: whalereinv1.OnlineStatus_failed, ErrorCode: whalereinv1.ErrorCode_internal,
		ChannelId: "channel-1", ResultReference: reference, RelayVersion: RelayEpoch,
	}
	if err := f.service.HandleHostResult(context.Background(), "h1", result); err != nil {
		t.Fatalf("unreadable reference discarded the outcome: %v", err)
	}
	if len(f.store.applyCalls) != 1 {
		t.Fatalf("apply calls = %d", len(f.store.applyCalls))
	}
	update := f.store.applyCalls[0]
	if update.Status != whalereinv1.OnlineStatus_failed || update.ErrorCode != whalereinv1.ErrorCode_internal {
		t.Fatalf("outcome changed: %+v", update)
	}
	if !bytes.Equal(update.ResultReference, mustMarshal(t, reference)) {
		t.Fatal("structurally valid reference was not persisted opaquely")
	}
}

func TestHandleHostResultRejectsBothRepresentations(t *testing.T) {
	f := newFixture(t)
	f.store.rows["r1"] = store.Request{RequestID: "r1", HostID: "h1", DeviceID: "d1", Status: whalereinv1.OnlineStatus_received}
	result := &whalereinv1.HostResult{
		RequestId: "r1", Status: whalereinv1.OnlineStatus_succeeded,
		ResultBody:      []byte("inline"),
		ResultReference: testReference(whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_RESULT, []byte("referenced")),
		RelayVersion:    RelayEpoch,
	}
	if err := f.service.HandleHostResult(context.Background(), "h1", result); !errors.Is(err, ErrInvalidResult) {
		t.Fatalf("both representations = %v", err)
	}
	if len(f.store.applyCalls) != 0 {
		t.Fatal("invalid result reached durable state")
	}
}

func TestPublishDueNoticesPublishesLatestStateAndRetries(t *testing.T) {
	f := newFixture(t)
	updated := f.now.Add(-time.Second)
	reference := testReference(whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_RESULT, []byte("result"))
	row := store.Request{
		RequestID: "r1", HostID: "h1", DeviceID: "d1", CommandID: "c1",
		Status: whalereinv1.OnlineStatus_succeeded, NoticeStatus: whalereinv1.OnlineStatus_succeeded,
		NoticePending: true, NoticeAttempts: 1, NoticeUpdatedAt: &updated,
		ResultReference: mustMarshal(t, reference),
	}
	f.store.noticeRows = []store.Request{row}
	if err := f.service.PublishDueNotices(context.Background()); err != nil {
		t.Fatal(err)
	}
	if len(f.broker.notices) != 1 {
		t.Fatalf("notices = %d", len(f.broker.notices))
	}
	notice := f.broker.notices[0]
	if notice.GetStatus() != whalereinv1.OnlineStatus_succeeded || notice.GetCommandId() != "c1" ||
		notice.GetRelayVersion() != RelayEpoch || notice.GetResultReference() == nil {
		t.Fatalf("notice = %+v", notice)
	}
	if len(f.store.noticePublished) != 1 {
		t.Fatal("publication was not recorded durably")
	}

	// A failed publication keeps the intent pending and schedules a retry.
	f = newFixture(t)
	f.store.noticeRows = []store.Request{row}
	f.broker.noticeErr = errors.New("mqtt down")
	if err := f.service.PublishDueNotices(context.Background()); err != nil {
		t.Fatal(err)
	}
	if len(f.store.noticeFailures) != 1 || len(f.store.noticePublished) != 0 {
		t.Fatal("failed publication was not retained for retry")
	}
}

func TestHeartbeatMapsNonRenewalToTypedOutcomes(t *testing.T) {
	cases := []struct {
		failure store.HeartbeatFailure
		want    error
	}{
		{store.HeartbeatNotFound, ErrNotFound},
		{store.HeartbeatDeviceForeign, ErrNotFound},
		{store.HeartbeatHardDeadline, ErrExpired},
		{store.HeartbeatLeaseElapsed, ErrExpired},
		{store.HeartbeatTerminal, ErrConflict},
		{store.HeartbeatSessionEnded, ErrConflict},
	}
	for _, tc := range cases {
		f := newFixture(t)
		f.store.heartbeatOutcome = store.HeartbeatOutcome{Failure: tc.failure}
		if _, err := f.service.Heartbeat(context.Background(), "d1", "s1", "r1"); !errors.Is(err, tc.want) {
			t.Fatalf("%s = %v, want %v", tc.failure, err, tc.want)
		}
	}
	f := newFixture(t)
	f.store.rows["r1"] = store.Request{RequestID: "r1", DeviceID: "d1"}
	row, err := f.service.Heartbeat(context.Background(), "d1", "s1", "r1")
	if err != nil || row.RequestID != "r1" {
		t.Fatalf("renewal = %+v err=%v", row, err)
	}
}

func mustMarshal(t *testing.T, message proto.Message) []byte {
	t.Helper()
	data, err := proto.Marshal(message)
	if err != nil {
		t.Fatal(err)
	}
	return data
}

func TestNoticeRetryDelayIsCapped(t *testing.T) {
	if NoticeRetryDelay(0) != 500*time.Millisecond {
		t.Fatal("zero attempt delay")
	}
	if NoticeRetryDelay(1) != 500*time.Millisecond {
		t.Fatal("first attempt delay")
	}
	if NoticeRetryDelay(100) != 16*time.Second && NoticeRetryDelay(100) != 30*time.Second {
		// Six doublings from 500ms is 16s; the explicit cap is 30s. Either
		// bounded value is acceptable, an unbounded one is not.
		t.Fatalf("delay = %s", NoticeRetryDelay(100))
	}
	if NoticeRetryDelay(100) > 30*time.Second {
		t.Fatal("retry delay is unbounded")
	}
}

func TestRunNoticesStopsImmediatelyOnCancellation(t *testing.T) {
	f := newFixture(t)
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if err := f.service.RunNotices(ctx); err != nil {
		t.Fatal(err)
	}
}

func TestPublishDueNoticesSkipsDeviceLessRowsAndReportsMarkFailures(t *testing.T) {
	f := newFixture(t)
	updated := f.now
	f.store.noticeRows = []store.Request{{
		RequestID: "r1", HostID: "h1", Status: whalereinv1.OnlineStatus_succeeded,
		NoticeStatus: whalereinv1.OnlineStatus_succeeded, NoticePending: true, NoticeUpdatedAt: &updated,
	}}
	if err := f.service.PublishDueNotices(context.Background()); err != nil {
		t.Fatal(err)
	}
	if len(f.store.noticePublished) != 1 || len(f.broker.notices) != 0 {
		t.Fatal("device-less row was not recorded as handled")
	}

	f = newFixture(t)
	f.store.noticeRows = []store.Request{{
		RequestID: "r1", HostID: "h1", DeviceID: "d1", Status: whalereinv1.OnlineStatus_succeeded,
		NoticeStatus: whalereinv1.OnlineStatus_succeeded, NoticePending: true, NoticeUpdatedAt: &updated,
	}}
	f.store.noticeMarkErr = errors.New("bookkeeping failed")
	if err := f.service.PublishDueNotices(context.Background()); err != nil {
		t.Fatal(err)
	}
	if len(f.broker.notices) != 1 {
		t.Fatal("notice not published")
	}
}

func TestReferenceValidationRejectsMalformedStructures(t *testing.T) {
	f := newFixture(t)
	base := testReference(whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_REQUEST, []byte("payload"))
	cases := []struct {
		name   string
		mutate func(*whalereinv1.PayloadReference, []byte)
		want   error
	}{
		{"bad object id", func(r *whalereinv1.PayloadReference, _ []byte) { r.ObjectId = "BAD" }, ErrInvalidRequest},
		{"empty owner", func(r *whalereinv1.PayloadReference, _ []byte) { r.OwnerNodeId = "" }, ErrInvalidRequest},
		{"zero length", func(r *whalereinv1.PayloadReference, _ []byte) { r.CiphertextBytes = 0 }, ErrInvalidRequest},
		{"bad digest length", func(r *whalereinv1.PayloadReference, _ []byte) { r.Sha256 = []byte{1} }, ErrInvalidRequest},
		{"zero expiry", func(r *whalereinv1.PayloadReference, _ []byte) { r.ExpiresAtUnixMs = 0 }, ErrInvalidRequest},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			reference := proto.Clone(base).(*whalereinv1.PayloadReference)
			tc.mutate(reference, nil)
			if err := f.service.validateReference(reference, whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_REQUEST, nil, 0); !errors.Is(err, tc.want) {
				t.Fatalf("err = %v", err)
			}
		})
	}
	f.service.cfg.PayloadOwnerID = "node-1"
	foreign := proto.Clone(base).(*whalereinv1.PayloadReference)
	foreign.OwnerNodeId = "other"
	if err := f.service.validateReference(foreign, whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_REQUEST, nil, 0); !errors.Is(err, ErrInvalidRequest) {
		t.Fatalf("foreign owner = %v", err)
	}
}

func TestReferenceChecksRequireAConfiguredChecker(t *testing.T) {
	f := newFixture(t)
	f.service.payloads = nil
	reference := testReference(whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_REQUEST, []byte("payload"))
	if err := f.service.checkReference(context.Background(), reference, store.PayloadCheckScope{}); !errors.Is(err, ErrUnavailable) {
		t.Fatalf("nil checker = %v", err)
	}
	if err := f.service.checkReference(context.Background(), nil, store.PayloadCheckScope{}); err != nil {
		t.Fatal(err)
	}
	if _, err := decodeReference([]byte("not protobuf")); !errors.Is(err, ErrInvalidResult) {
		t.Fatalf("corrupt reference = %v", err)
	}
	if reference, err := decodeReference(nil); err != nil || reference != nil {
		t.Fatalf("empty reference = %+v err=%v", reference, err)
	}
}

func TestUnreadableResultReferenceIsWithheldFromRelease(t *testing.T) {
	f := newFixture(t)
	f.store.rows["r1"] = store.Request{
		RequestID: "r1", HostID: "h1", DeviceID: "d1", Status: whalereinv1.OnlineStatus_succeeded,
		ResultReference: []byte("corrupt"),
	}
	f.store.releaseOutcome = store.ReleaseOutcome{
		Request: f.store.rows["r1"], SessionValid: true, StampPresent: true,
		GrantActive: true, StampCurrent: true, WithinResultBound: true, WithinDelivery: true,
	}
	row, err := f.service.Release(context.Background(), "d1", "", "r1", 1<<20)
	if err != nil {
		t.Fatal(err)
	}
	if row.ResultReference != nil {
		t.Fatal("corrupt reference was returned")
	}
}

func TestHeartbeatStoreFailureIsReported(t *testing.T) {
	f := newFixture(t)
	f.store.heartbeatErr = errors.New("db down")
	if _, err := f.service.Heartbeat(context.Background(), "d1", "", "r1"); err == nil {
		t.Fatal("store failure was hidden")
	}
}

func TestForwardedSizeCheckIsOptionalAndReferenceLengthMustMatch(t *testing.T) {
	f := newFixture(t)
	f.service.cfg.EnvelopeBytes = 0
	if err := f.service.checkForwardedSize(store.Request{}); err != nil {
		t.Fatalf("disabled envelope bound = %v", err)
	}
	f.service.cfg.EnvelopeBytes = 1
	if err := f.service.checkForwardedSize(store.Request{RequestID: "r1"}); !errors.Is(err, ErrRequestTooLarge) {
		t.Fatalf("oversized envelope = %v", err)
	}
	reference := testReference(whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_REQUEST, []byte("payload"))
	if err := f.service.validateReference(reference, whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_REQUEST, nil, 1); !errors.Is(err, ErrInvalidRequest) {
		t.Fatalf("length mismatch = %v", err)
	}
	if err := f.service.validateReference(reference, whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_RESULT, nil, 0); !errors.Is(err, ErrInvalidRequest) {
		t.Fatalf("purpose mismatch = %v", err)
	}
}
