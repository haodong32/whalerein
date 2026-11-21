package requests

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"log/slog"
	"strings"
	"testing"
	"time"

	"github.com/prometheus/client_golang/prometheus/testutil"
	"github.com/whalerein/whalerein/access/internal/broker"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

type payloadCheckCall struct {
	reference *whalereinv1.PayloadReference
	scope     store.PayloadCheckScope
}

type fakePayloadChecker struct {
	err   error
	calls []payloadCheckCall
}

func (f *fakePayloadChecker) CheckPayload(_ context.Context, reference *whalereinv1.PayloadReference, scope store.PayloadCheckScope) error {
	f.calls = append(f.calls, payloadCheckCall{reference: reference, scope: scope})
	return f.err
}

type fakeGrantAuthority struct {
	grants map[string]store.AdmissionGrant
	err    error
	calls  []string
}

func (f *fakeGrantAuthority) AdmissionGrantFor(_ context.Context, hostID, deviceID string) (store.AdmissionGrant, error) {
	f.calls = append(f.calls, hostID+"|"+deviceID)
	if f.err != nil {
		return store.AdmissionGrant{}, f.err
	}
	grant, ok := f.grants[hostID+"|"+deviceID]
	if !ok {
		return store.AdmissionGrant{}, store.ErrNotFound
	}
	return grant, nil
}

type fakeStore struct {
	rows           map[string]store.Request
	createOverride func(store.Request) (store.CreateOutcome, error)
	createErr      error
	getErr         error
	claimRows      []store.Request
	claimErr       error
	requeueCount   int64
	requeueErr     error
	published      []string
	publishErr     error
	failures       []string
	failureErr     error
	applyCalls     []store.HostResultUpdate
	applyResult    *bool
	applyErr       error

	renewCalls    []renewCall
	renewResult   bool
	renewErr      error
	expireRows    []store.Request
	expireErr     error
	expireLimit   int
	pendingRows   []store.Request
	pendingErr    error
	cancelMarked  []string
	cancelMarkErr error
	attemptMarked []string
	attemptErr    error

	releaseOutcome store.ReleaseOutcome
	releaseErr     error
	releaseScopes  []store.ReleaseScope

	heartbeatOutcome store.HeartbeatOutcome
	heartbeatErr     error
	heartbeatCalls   []renewCall

	noticeRows      []store.Request
	noticeClaimErr  error
	noticePublished []noticePublishCall
	noticeFailures  []string
	noticeMarkErr   error
	noticeStats     store.NoticeBacklog
	noticeStatsErr  error
}

type noticePublishCall struct {
	requestID string
	status    whalereinv1.OnlineStatus
	updatedAt time.Time
}

type renewCall struct {
	deviceID  string
	requestID string
	lease     time.Duration
}

func newFakeStore() *fakeStore { return &fakeStore{rows: map[string]store.Request{}} }

func (f *fakeStore) CreateRequest(_ context.Context, req store.Request) (store.CreateOutcome, error) {
	if f.createOverride != nil {
		return f.createOverride(req)
	}
	if f.createErr != nil {
		return store.CreateOutcome{}, f.createErr
	}
	if existing, ok := f.rows[req.RequestID]; ok {
		return store.CreateOutcome{Request: existing}, nil
	}
	f.rows[req.RequestID] = req
	return store.CreateOutcome{Request: req, Created: true}, nil
}

func (f *fakeStore) ReleaseRequest(_ context.Context, scope store.ReleaseScope) (store.ReleaseOutcome, error) {
	f.releaseScopes = append(f.releaseScopes, scope)
	if f.releaseErr != nil {
		return store.ReleaseOutcome{}, f.releaseErr
	}
	if f.releaseOutcome.Request.RequestID == "" {
		row, ok := f.rows[scope.RequestID]
		if !ok || row.DeviceID != scope.DeviceID {
			return store.ReleaseOutcome{}, store.ErrNotFound
		}
		return store.ReleaseOutcome{Request: row, SessionValid: true, StampPresent: true, GrantActive: true, StampCurrent: true, WithinResultBound: true, WithinDelivery: true}, nil
	}
	return f.releaseOutcome, nil
}

func (f *fakeStore) GetRequest(_ context.Context, requestID string) (store.Request, error) {
	if f.getErr != nil {
		return store.Request{}, f.getErr
	}
	row, ok := f.rows[requestID]
	if !ok {
		return store.Request{}, store.ErrNotFound
	}
	return row, nil
}

func (f *fakeStore) ClaimDueRequests(_ context.Context, _ time.Time, _ int, _ time.Duration) ([]store.Request, error) {
	return f.claimRows, f.claimErr
}

func (f *fakeStore) RequeueDuePublished(_ context.Context, _ time.Time, _ int) (int64, error) {
	return f.requeueCount, f.requeueErr
}

func (f *fakeStore) MarkPublished(_ context.Context, requestID string, _ time.Time, _ time.Duration) error {
	if f.publishErr != nil {
		return f.publishErr
	}
	f.published = append(f.published, requestID)
	if row, ok := f.rows[requestID]; ok {
		row.ForwardState = store.ForwardPublished
		row.PublishedOnce = true
		f.rows[requestID] = row
	}
	return nil
}

func (f *fakeStore) MarkPublishFailure(_ context.Context, requestID string, _ time.Time, _ time.Duration, _ int) error {
	if f.failureErr != nil {
		return f.failureErr
	}
	f.failures = append(f.failures, requestID)
	return nil
}

func (f *fakeStore) ApplyHostResult(_ context.Context, update store.HostResultUpdate) (bool, error) {
	if f.applyErr != nil {
		return false, f.applyErr
	}
	f.applyCalls = append(f.applyCalls, update)
	if f.applyResult != nil {
		return *f.applyResult, nil
	}
	return true, nil
}

func (f *fakeStore) RenewClientLease(_ context.Context, deviceID, requestID string, lease time.Duration) (bool, error) {
	f.renewCalls = append(f.renewCalls, renewCall{deviceID: deviceID, requestID: requestID, lease: lease})
	if f.renewErr != nil {
		return false, f.renewErr
	}
	return f.renewResult, nil
}

func (f *fakeStore) HeartbeatRequest(_ context.Context, deviceID, sessionID, requestID string, lease time.Duration) (store.HeartbeatOutcome, error) {
	f.heartbeatCalls = append(f.heartbeatCalls, renewCall{deviceID: deviceID, requestID: requestID, lease: lease})
	if f.heartbeatErr != nil {
		return store.HeartbeatOutcome{}, f.heartbeatErr
	}
	if f.heartbeatOutcome.Request.RequestID == "" && f.heartbeatOutcome.Failure == "" {
		return store.HeartbeatOutcome{Renewed: true, Request: f.rows[requestID]}, nil
	}
	return f.heartbeatOutcome, nil
}

func (f *fakeStore) ClaimNoticeIntents(_ context.Context, _ time.Time, _ int, _ time.Duration) ([]store.Request, error) {
	return f.noticeRows, f.noticeClaimErr
}

func (f *fakeStore) MarkNoticePublished(_ context.Context, requestID string, status whalereinv1.OnlineStatus, updatedAt time.Time, _ time.Time) (bool, error) {
	if f.noticeMarkErr != nil {
		return false, f.noticeMarkErr
	}
	f.noticePublished = append(f.noticePublished, noticePublishCall{requestID: requestID, status: status, updatedAt: updatedAt})
	return true, nil
}

func (f *fakeStore) MarkNoticeFailure(_ context.Context, requestID string, _, _ time.Time) error {
	f.noticeFailures = append(f.noticeFailures, requestID)
	return nil
}

func (f *fakeStore) NoticeStats(context.Context) (store.NoticeBacklog, error) {
	return f.noticeStats, f.noticeStatsErr
}

func (f *fakeStore) ExpireClientLeases(_ context.Context, limit int) ([]store.Request, error) {
	f.expireLimit = limit
	if f.expireErr != nil {
		return nil, f.expireErr
	}
	return f.expireRows, nil
}

func (f *fakeStore) PendingCancelRequests(_ context.Context, _ int) ([]store.Request, error) {
	if f.pendingErr != nil {
		return nil, f.pendingErr
	}
	return f.pendingRows, nil
}

func (f *fakeStore) MarkCancelPublished(_ context.Context, requestID string) error {
	if f.cancelMarkErr != nil {
		return f.cancelMarkErr
	}
	f.cancelMarked = append(f.cancelMarked, requestID)
	return nil
}

func (f *fakeStore) MarkCancelAttempt(_ context.Context, requestID string) error {
	if f.attemptErr != nil {
		return f.attemptErr
	}
	f.attemptMarked = append(f.attemptMarked, requestID)
	return nil
}

type fakeBroker struct {
	published []*whalereinv1.HostRequest
	hosts     []string
	presence  []string
	notices   []*whalereinv1.RequestStateNotice
	noticeErr error
	err       error
}

func (f *fakeBroker) PublishRequest(_ context.Context, hostID string, request *whalereinv1.HostRequest) error {
	f.hosts = append(f.hosts, hostID)
	f.published = append(f.published, request)
	return f.err
}

func (f *fakeBroker) PublishPresence(_ context.Context, hostID string, payload []byte) error {
	f.presence = append(f.presence, hostID+"|"+string(payload))
	return f.err
}

func (f *fakeBroker) PublishDeviceNotice(_ context.Context, _, _ string, notice *whalereinv1.RequestStateNotice) error {
	f.notices = append(f.notices, notice)
	return f.noticeErr
}

type fakePresence struct {
	online map[string]bool
	// observed models a reconciled presence view: a host missing here is
	// unknown rather than explicitly offline. A nil map keeps the legacy
	// behaviour for tests that only script the online set.
	observed  map[string]bool
	available *bool
}

func (f *fakePresence) IsHostOnline(_ context.Context, hostID string) bool {
	if f.observed != nil && !f.observed[hostID] {
		return false
	}
	return f.online[hostID]
}

func (f *fakePresence) Available() bool {
	if f.available != nil {
		return *f.available
	}
	return true
}

type fixture struct {
	service  *Service
	store    *fakeStore
	broker   *fakeBroker
	presence *fakePresence
	grants   *fakeGrantAuthority
	payloads *fakePayloadChecker
	now      time.Time
}

func newFixture(t *testing.T) *fixture {
	t.Helper()
	fs := newFakeStore()
	broker := &fakeBroker{}
	presence := &fakePresence{online: map[string]bool{"h1": true}}
	grants := &fakeGrantAuthority{grants: map[string]store.AdmissionGrant{
		"h1|d1": {HostID: "h1", DeviceID: "d1", Active: true, GrantGeneration: 7, RevocationGeneration: 3, DeviceRevocationGeneration: 2},
	}}
	payloads := &fakePayloadChecker{}
	now := time.Date(2026, 11, 9, 12, 0, 0, 0, time.UTC)
	ref := &fixture{store: fs, broker: broker, presence: presence, grants: grants, payloads: payloads, now: now}
	service, err := New(Dependencies{
		Store: fs, Broker: broker, Presence: presence, Grants: grants, Payloads: payloads,
		Config: Config{
			ProtocolVersion: 1, MaxTTL: time.Hour, ForwardInterval: time.Second,
			Lease: 30 * time.Second, RetryInterval: 5 * time.Second, MaxAttempts: 3, BatchSize: 10,
		},
		Metrics: diagnostics.NewMetrics(),
		Masker:  diagnostics.NewMaskerWithKey([]byte("req")),
		Logger:  slog.New(slog.NewTextHandler(io.Discard, nil)),
		Now:     func() time.Time { return ref.now },
	})
	if err != nil {
		t.Fatal(err)
	}
	ref.service = service
	return ref
}

func validRequest(now time.Time, commandID string) *whalereinv1.HostRequest {
	return &whalereinv1.HostRequest{
		RequestId:       "req-1",
		HostId:          "h1",
		DeviceId:        "d1",
		Kind:            whalereinv1.RequestKind_REQUEST_KIND_COMMAND,
		CommandId:       commandID,
		PayloadDigest:   []byte("digest"),
		ExpiresAtUnixMs: now.Add(5 * time.Minute).UnixMilli(),
		ProtocolVersion: 1,
		RelayVersion:    3,
		ChannelId:       "channel-1",
		TraceId:         "trace-1",
		Body:            []byte("ciphertext"),
	}
}

func input(request *whalereinv1.HostRequest) SubmitInput {
	return SubmitInput{DeviceID: "d1", HostID: "h1", Request: request}
}

func TestNewRejectsMissingRequiredDependencies(t *testing.T) {
	if _, err := New(Dependencies{}); err == nil {
		t.Fatal("expected error")
	}
	if _, err := New(Dependencies{Store: newFakeStore()}); err == nil {
		t.Fatal("expected broker error")
	}
	if _, err := New(Dependencies{Store: newFakeStore(), Broker: &fakeBroker{}}); err == nil {
		t.Fatal("expected presence error")
	}
	if _, err := New(Dependencies{Store: newFakeStore(), Broker: &fakeBroker{}, Presence: &fakePresence{online: map[string]bool{}}}); err == nil {
		t.Fatal("expected grant authority error")
	}
}

func TestValidateRejectsInvalidRequestsAndAcceptsValidOnes(t *testing.T) {
	f := newFixture(t)
	now := f.now
	cases := []struct {
		name    string
		mutate  func(*whalereinv1.HostRequest)
		input   *SubmitInput
		wantErr error
	}{
		{"rejects missing request body", nil, &SubmitInput{DeviceID: "d1", HostID: "h1"}, ErrInvalidRequest},
		{"rejects missing device ID", nil, &SubmitInput{HostID: "h1", Request: validRequest(now, "c")}, ErrInvalidRequest},
		{"rejects missing host ID", nil, &SubmitInput{DeviceID: "d1", Request: validRequest(now, "c")}, ErrInvalidRequest},
		{"rejects missing request ID", func(r *whalereinv1.HostRequest) { r.RequestId = "" }, nil, ErrInvalidRequest},
		{"rejects request addressed to a different host", func(r *whalereinv1.HostRequest) { r.HostId = "h2" }, nil, ErrInvalidRequest},
		{"rejects request submitted by a different device", func(r *whalereinv1.HostRequest) { r.DeviceId = "d2" }, nil, ErrForbidden},
		{"rejects unsupported protocol version", func(r *whalereinv1.HostRequest) { r.ProtocolVersion = 2 }, nil, ErrProtocolVersion},
		{"rejects expired request", func(r *whalereinv1.HostRequest) { r.ExpiresAtUnixMs = now.Add(-time.Second).UnixMilli() }, nil, ErrExpired},
		{"rejects request beyond maximum lifetime", func(r *whalereinv1.HostRequest) { r.ExpiresAtUnixMs = now.Add(2 * time.Hour).UnixMilli() }, nil, ErrInvalidRequest},
		{"rejects command identity on handshake", func(r *whalereinv1.HostRequest) {
			r.Kind = whalereinv1.RequestKind_REQUEST_KIND_HANDSHAKE
			r.CommandId = "c"
		}, nil, ErrInvalidRequest},
		{"rejects digest on query", func(r *whalereinv1.HostRequest) {
			r.Kind = whalereinv1.RequestKind_REQUEST_KIND_QUERY
			r.PayloadDigest = []byte("d")
		}, nil, ErrInvalidRequest},
		{"rejects command without command ID", func(r *whalereinv1.HostRequest) { r.CommandId = "" }, nil, ErrInvalidRequest},
		{"rejects command without payload digest", func(r *whalereinv1.HostRequest) { r.PayloadDigest = nil }, nil, ErrInvalidRequest},
		{"rejects pairing claim in request submission", func(r *whalereinv1.HostRequest) { r.Kind = whalereinv1.RequestKind_REQUEST_KIND_PAIRING_CLAIM }, nil, ErrInvalidRequest},
		{"rejects revocation notice in request submission", func(r *whalereinv1.HostRequest) { r.Kind = whalereinv1.RequestKind_REQUEST_KIND_REVOCATION }, nil, ErrInvalidRequest},
		{"rejects internal Host client listing in request submission", func(r *whalereinv1.HostRequest) { r.Kind = whalereinv1.RequestKind_REQUEST_KIND_HOST_CLIENTS }, nil, ErrInvalidRequest},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			request := validRequest(now, "c")
			if tc.mutate != nil {
				tc.mutate(request)
			}
			submit := input(request)
			if tc.input != nil {
				submit = *tc.input
			}
			_, _, err := f.service.Submit(context.Background(), submit)
			if !errors.Is(err, tc.wantErr) {
				t.Fatalf("err = %v, want %v", err, tc.wantErr)
			}
		})
	}
}

func TestSubmitMarksOfflineHostWithoutPublishing(t *testing.T) {
	f := newFixture(t)
	f.presence.online["h1"] = false
	request := validRequest(f.now, "c1")
	request.Kind = whalereinv1.RequestKind_REQUEST_KIND_HANDSHAKE
	request.CommandId = ""
	request.PayloadDigest = nil
	row, created, err := f.service.Submit(context.Background(), input(request))
	if err != nil || !created {
		t.Fatalf("row=%+v created=%v err=%v", row, created, err)
	}
	if row.Status != whalereinv1.OnlineStatus_host_offline || row.ForwardState != store.ForwardNone {
		t.Fatalf("row = %+v", row)
	}
	if len(f.broker.published) != 0 {
		t.Fatal("host offline request was published")
	}
}

func TestSubmitQueuesWhilePresenceIsUnavailableInsteadOfClosingHostOffline(t *testing.T) {
	f := newFixture(t)
	unavailable := false
	f.presence.available = &unavailable
	f.presence.online["h1"] = false // an unavailable view must not be read as offline
	request := validRequest(f.now, "c1")
	row, created, err := f.service.Submit(context.Background(), input(request))
	if err != nil || !created {
		t.Fatalf("row=%+v created=%v err=%v", row, created, err)
	}
	if row.Status != whalereinv1.OnlineStatus_received || row.ForwardState != store.ForwardPending {
		t.Fatalf("unavailable presence closed the request: %+v", row)
	}
	if len(f.broker.published) != 0 || len(f.store.applyCalls) != 0 {
		t.Fatal("request was published or finalized while presence was unavailable")
	}

	// Once broker state is established and the host is online, the retry loop
	// forwards the queued request exactly once.
	available := true
	f.presence.available = &available
	f.presence.online["h1"] = true
	f.store.claimRows = []store.Request{row}
	if err := f.service.ForwardDue(context.Background()); err != nil {
		t.Fatal(err)
	}
	if len(f.broker.published) != 1 || len(f.store.published) != 1 {
		t.Fatalf("queued request was not forwarded: broker=%d store=%d", len(f.broker.published), len(f.store.published))
	}
}

func TestForwardLoopSkipsUnavailablePresenceAndClosesKnownOfflineHost(t *testing.T) {
	f := newFixture(t)
	queued := store.Request{
		RequestID: "r1", HostID: "h1", DeviceID: "d1", CommandID: "c1",
		Kind: whalereinv1.RequestKind_REQUEST_KIND_COMMAND, Status: whalereinv1.OnlineStatus_received,
		ForwardState: store.ForwardPending, ExpiresAt: f.now.Add(time.Minute),
	}
	f.store.claimRows = []store.Request{queued}
	unavailable := false
	f.presence.available = &unavailable
	if err := f.service.ForwardDue(context.Background()); err != nil {
		t.Fatal(err)
	}
	if len(f.broker.published) != 0 || len(f.store.applyCalls) != 0 {
		t.Fatal("unavailable presence published or finalized a queued request")
	}

	available := true
	f.presence.available = &available
	f.presence.online["h1"] = false
	if err := f.service.ForwardDue(context.Background()); err != nil {
		t.Fatal(err)
	}
	if len(f.broker.published) != 0 {
		t.Fatal("known-offline host received a publish")
	}
	if len(f.store.applyCalls) != 1 || f.store.applyCalls[0].Status != whalereinv1.OnlineStatus_host_offline {
		t.Fatalf("known-offline request was not closed as host_offline: %+v", f.store.applyCalls)
	}
}

func TestForwardLoopRetainsPairingCommitWhileHostIsOffline(t *testing.T) {
	f := newFixture(t)
	f.presence.online["h1"] = false
	f.store.claimRows = []store.Request{{
		RequestID: "pair-1", HostID: "h1", DeviceID: "d1",
		Kind:   whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE,
		Status: whalereinv1.OnlineStatus_received, ForwardState: store.ForwardPending,
		ExpiresAt: f.now.Add(time.Minute),
	}}
	if err := f.service.ForwardDue(context.Background()); err != nil {
		t.Fatal(err)
	}
	if len(f.broker.published) != 0 {
		t.Fatal("pairing commit was published to an offline host")
	}
	if len(f.store.applyCalls) != 0 {
		t.Fatal("pairing commit was finalized while the host was offline")
	}
}

func TestSubmitPersistsRequestBeforePublishingOnlineHost(t *testing.T) {
	f := newFixture(t)
	request := validRequest(f.now, "c1")
	stored := store.Request{
		RequestID: request.RequestId, HostID: "h1", DeviceID: "d1", CommandID: "c1",
		PayloadDigest: request.PayloadDigest, Kind: request.Kind, ProtocolVersion: 1,
		Status: whalereinv1.OnlineStatus_received, RequestBody: request.Body,
		ExpiresAt: time.UnixMilli(request.ExpiresAtUnixMs), ForwardState: store.ForwardPending,
		ForwardAttempts: 1,
	}
	f.store.createOverride = func(req store.Request) (store.CreateOutcome, error) {
		req.ForwardAttempts = 1
		return store.CreateOutcome{Request: req, Created: true}, nil
	}
	f.store.claimRows = []store.Request{stored}
	row, created, err := f.service.Submit(context.Background(), input(request))
	if err != nil || !created {
		t.Fatalf("row=%+v created=%v err=%v", row, created, err)
	}
	if row.Status != whalereinv1.OnlineStatus_received {
		t.Fatalf("status = %v", row.Status)
	}
	if len(f.broker.published) != 1 {
		t.Fatalf("published = %d", len(f.broker.published))
	}
	published := f.broker.published[0]
	if published.GetRequestId() != "req-1" || published.GetHostId() != "h1" || published.GetKind() != whalereinv1.RequestKind_REQUEST_KIND_COMMAND ||
		!bytes.Equal(published.GetBody(), []byte("ciphertext")) || !bytes.Equal(published.GetPayloadDigest(), []byte("digest")) {
		t.Fatalf("published = %+v", published)
	}
	if len(f.store.published) != 1 || f.store.published[0] != "req-1" {
		t.Fatalf("marked = %v", f.store.published)
	}
}

func TestSubmitDeduplicatesMatchingCommandAndRejectsChangedDigest(t *testing.T) {
	f := newFixture(t)
	existing := store.Request{RequestID: "req-old", HostID: "h1", DeviceID: "d1", CommandID: "c1", Status: whalereinv1.OnlineStatus_succeeded, ExpiresAt: f.now.Add(time.Hour)}
	request := validRequest(f.now, "c1")
	request.RequestId = "req-new"
	f.store.createOverride = func(req store.Request) (store.CreateOutcome, error) {
		return store.CreateOutcome{Request: existing}, nil
	}
	row, created, err := f.service.Submit(context.Background(), input(request))
	if err != nil || created || row.RequestID != "req-old" {
		t.Fatalf("row=%+v created=%v err=%v", row, created, err)
	}
	if len(f.broker.published) != 0 {
		t.Fatal("deduped request republished")
	}

	f.store.createOverride = func(req store.Request) (store.CreateOutcome, error) {
		return store.CreateOutcome{Request: existing, DigestMismatch: true}, nil
	}
	if _, _, err := f.service.Submit(context.Background(), input(request)); !errors.Is(err, ErrDigestMismatch) {
		t.Fatalf("digest err = %v", err)
	}

	f.store.createOverride = func(req store.Request) (store.CreateOutcome, error) {
		return store.CreateOutcome{}, store.ErrConflict
	}
	if _, _, err := f.service.Submit(context.Background(), input(request)); !errors.Is(err, ErrConflict) {
		t.Fatalf("conflict err = %v", err)
	}

	f.store.createOverride = func(req store.Request) (store.CreateOutcome, error) {
		return store.CreateOutcome{}, errors.New("db down")
	}
	if _, _, err := f.service.Submit(context.Background(), input(request)); err == nil {
		t.Fatal("expected store error")
	}
}

func TestForwardDueRequestsRetriesAndHandlesPublishFailures(t *testing.T) {
	f := newFixture(t)
	f.store.requeueCount = 2
	f.store.claimRows = []store.Request{{
		RequestID: "r1", HostID: "h1", DeviceID: "d1", Kind: whalereinv1.RequestKind_REQUEST_KIND_COMMAND,
		ExpiresAt: f.now.Add(time.Hour), ForwardAttempts: 1, Status: whalereinv1.OnlineStatus_received,
	}}
	if err := f.service.ForwardDue(context.Background()); err != nil {
		t.Fatal(err)
	}
	if len(f.broker.published) != 1 || len(f.store.published) != 1 || f.store.published[0] != "r1" || testutil.ToFloat64(f.service.metrics.RequestForwardRetries) != 2 {
		t.Fatal("forwarding did not publish, persist, and count retries")
	}
	f.broker.err = errors.New("mqtt down")
	if err := f.service.ForwardDue(context.Background()); err != nil {
		t.Fatal(err)
	}
	if len(f.store.failures) != 1 {
		t.Fatalf("failures = %v", f.store.failures)
	}
	f.store.failureErr = errors.New("db down")
	if err := f.service.ForwardDue(context.Background()); err == nil {
		t.Fatal("expected failure mark error")
	}
	f.store.failureErr = nil
	f.store.publishErr = errors.New("db down")
	f.broker.err = nil
	if err := f.service.ForwardDue(context.Background()); err == nil {
		t.Fatal("expected published mark error")
	}
	f.store.publishErr = nil
	f.store.requeueErr = errors.New("db down")
	if err := f.service.ForwardDue(context.Background()); err == nil {
		t.Fatal("expected requeue error")
	}
	f.store.requeueErr = nil
	f.store.claimErr = errors.New("db down")
	if err := f.service.ForwardDue(context.Background()); err == nil {
		t.Fatal("expected claim error")
	}
	f.store.claimErr = nil
	// A cancelled context stops before publishing the next row.
	f.store.claimRows = []store.Request{{RequestID: "r2", HostID: "h1", DeviceID: "d1", ExpiresAt: f.now.Add(time.Hour)}}
	cancelled, cancel := context.WithCancel(context.Background())
	cancel()
	before := len(f.broker.published)
	if err := f.service.ForwardDue(cancelled); err != nil {
		t.Fatalf("cancelled forward = %v", err)
	}
	if len(f.broker.published) != before {
		t.Fatal("cancelled worker published its next request")
	}
}

func TestRunStopsForwardingWhenContextIsCanceled(t *testing.T) {
	f := newFixture(t)
	f.service.cfg.ForwardInterval = time.Millisecond
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { done <- f.service.Run(ctx) }()
	time.Sleep(5 * time.Millisecond)
	cancel()
	select {
	case err := <-done:
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(time.Second):
		t.Fatal("run did not stop")
	}
}

func TestGetRequestReturnsStoredRowAndPropagatesLookupErrors(t *testing.T) {
	f := newFixture(t)
	f.store.rows["r1"] = store.Request{
		RequestID: "r1", DeviceID: "d1",
		Status: whalereinv1.OnlineStatus_host_accepted, ExpiresAt: f.now.Add(time.Minute),
	}
	row, err := f.service.Get(context.Background(), "d1", "r1")
	if err != nil || row.RequestID != "r1" {
		t.Fatalf("row=%+v err=%v", row, err)
	}
	if _, err := f.service.Get(context.Background(), "d2", "r1"); !errors.Is(err, ErrNotFound) {
		t.Fatalf("wrong device = %v", err)
	}
	if _, err := f.service.Get(context.Background(), "d1", "missing"); !errors.Is(err, ErrNotFound) {
		t.Fatalf("missing = %v", err)
	}
	f.store.getErr = errors.New("db down")
	if _, err := f.service.Get(context.Background(), "d1", "r1"); err == nil {
		t.Fatal("expected store error")
	}
}

func TestGetRequestReportsExpiredNonTerminalRowsButKeepsTerminalRows(t *testing.T) {
	f := newFixture(t)
	f.store.rows["pending"] = store.Request{
		RequestID: "pending", DeviceID: "d1",
		Status: whalereinv1.OnlineStatus_host_accepted, ExpiresAt: f.now.Add(-time.Second),
	}
	if _, err := f.service.Get(context.Background(), "d1", "pending"); !errors.Is(err, ErrExpired) {
		t.Fatalf("expired pending request = %v", err)
	}
	f.store.rows["done"] = store.Request{
		RequestID: "done", DeviceID: "d1",
		Status: whalereinv1.OnlineStatus_succeeded, ExpiresAt: f.now.Add(-time.Second),
	}
	if row, err := f.service.Get(context.Background(), "d1", "done"); err != nil || row.RequestID != "done" {
		t.Fatalf("terminal row after expiry = %+v, %v", row, err)
	}
}

func TestHandleHostResultAppliesOnlyValidStatusTransitions(t *testing.T) {
	f := newFixture(t)
	row := store.Request{
		RequestID: "r1", HostID: "h1", DeviceID: "d1", CommandID: "c1",
		PayloadDigest: []byte("digest"), ChannelID: "channel-1", Status: whalereinv1.OnlineStatus_received,
	}
	f.store.rows["r1"] = row
	result := &whalereinv1.HostResult{
		RequestId: "r1", HostId: "h1", DeviceId: "d1", Status: whalereinv1.OnlineStatus_succeeded,
		CommandId: "c1", PayloadDigest: []byte("digest"), ResultBody: []byte("result"),
		ChannelId: "channel-1", ErrorCode: whalereinv1.ErrorCode_internal, ErrorMessage: "diag",
		RelayVersion: RelayEpoch,
	}
	if err := f.service.HandleHostResult(context.Background(), "h1", result); err != nil {
		t.Fatal(err)
	}
	if len(f.store.applyCalls) != 1 {
		t.Fatalf("apply calls = %d", len(f.store.applyCalls))
	}
	update := f.store.applyCalls[0]
	if update.HostID != "h1" || update.DeviceID != "d1" || !bytes.Equal(update.ResultBody, []byte("result")) || update.ChannelID != "channel-1" || update.ErrorCode != whalereinv1.ErrorCode_internal {
		t.Fatalf("update = %+v", update)
	}

	// Empty optional fields are accepted and filled from the row.
	result = &whalereinv1.HostResult{RequestId: "r1", Status: whalereinv1.OnlineStatus_host_accepted, RelayVersion: RelayEpoch}
	if err := f.service.HandleHostResult(context.Background(), "h1", result); err != nil {
		t.Fatal(err)
	}

	cases := []struct {
		name    string
		topic   string
		request *whalereinv1.HostResult
		wantErr error
	}{
		{"rejects nil host result", "h1", nil, ErrInvalidResult},
		{"rejects host result without request ID", "h1", &whalereinv1.HostResult{}, ErrInvalidResult},
		{"rejects host result without topic host", "", result, ErrInvalidResult},
		{"rejects host result for a different host", "h2", &whalereinv1.HostResult{RequestId: "r1", HostId: "h2", Status: whalereinv1.OnlineStatus_succeeded}, ErrResultMismatch},
		{"rejects host result when stored host differs", "h2", &whalereinv1.HostResult{RequestId: "r1", Status: whalereinv1.OnlineStatus_succeeded}, ErrResultMismatch},
		{"rejects host result for a different device", "h1", &whalereinv1.HostResult{RequestId: "r1", DeviceId: "d2", Status: whalereinv1.OnlineStatus_succeeded}, ErrResultMismatch},
		{"rejects host result with a different command ID", "h1", &whalereinv1.HostResult{RequestId: "r1", CommandId: "c2", Status: whalereinv1.OnlineStatus_succeeded}, ErrResultMismatch},
		{"rejects host result with a different payload digest", "h1", &whalereinv1.HostResult{RequestId: "r1", PayloadDigest: []byte("other"), Status: whalereinv1.OnlineStatus_succeeded}, ErrResultMismatch},
		{"rejects host result from a different secure channel", "h1", &whalereinv1.HostResult{RequestId: "r1", ChannelId: "channel-2", Status: whalereinv1.OnlineStatus_succeeded}, ErrResultMismatch},
		{"rejects unspecified host result status", "h1", &whalereinv1.HostResult{RequestId: "r1", Status: whalereinv1.OnlineStatus_online_status_unspecified}, ErrInvalidResult},
		{"rejects received as a host result status", "h1", &whalereinv1.HostResult{RequestId: "r1", Status: whalereinv1.OnlineStatus_received}, ErrInvalidResult},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := f.service.HandleHostResult(context.Background(), tc.topic, tc.request)
			if !errors.Is(err, tc.wantErr) {
				t.Fatalf("err = %v, want %v", err, tc.wantErr)
			}
		})
	}
	if err := f.service.HandleHostResult(context.Background(), "h1", &whalereinv1.HostResult{RequestId: "missing", Status: whalereinv1.OnlineStatus_succeeded}); !errors.Is(err, ErrNotFound) {
		t.Fatalf("missing = %v", err)
	}
	f.store.getErr = errors.New("db down")
	if err := f.service.HandleHostResult(context.Background(), "h1", result); err == nil {
		t.Fatal("expected get error")
	}
	f.store.getErr = nil
	f.store.applyErr = errors.New("db down")
	if err := f.service.HandleHostResult(context.Background(), "h1", result); err == nil {
		t.Fatal("expected apply error")
	}
	f.store.applyErr = nil
	applied := false
	f.store.applyResult = &applied
	if err := f.service.HandleHostResult(context.Background(), "h1", result); err != nil {
		t.Fatalf("non-applied result = %v", err)
	}
}

func TestSubmitWithoutOptionalMetricsRecordsOfflineAcceptanceAndInvalidRequestFailure(t *testing.T) {
	f := newFixture(t)
	service, err := New(Dependencies{Store: f.store, Broker: f.broker, Presence: f.presence, Grants: f.grants, Config: f.service.cfg, Logger: f.service.logger, Masker: f.service.masker, Now: func() time.Time { return f.now }})
	if err != nil {
		t.Fatal(err)
	}
	if service.metrics == nil {
		t.Fatal("omitted metrics were not defaulted")
	}
	f.presence.online["h1"] = false
	request := validRequest(f.now, "c1")
	row, _, err := service.Submit(context.Background(), input(request))
	if err != nil || row.Status != whalereinv1.OnlineStatus_host_offline || len(f.broker.published) != 0 {
		t.Fatal("offline acceptance changed status or published a command")
	}
	if _, _, err := service.Submit(context.Background(), input(nil)); !errors.Is(err, ErrInvalidRequest) {
		t.Fatal("missing request was not rejected")
	}
	if testutil.ToFloat64(service.metrics.HostRequestErrors) != 1 {
		t.Fatal("invalid request did not increment exactly one error observation")
	}
	families, err := service.metrics.Registry.Gather()
	if err != nil {
		t.Fatal("request metrics could not be gathered")
	}
	observed := uint64(0)
	for _, family := range families {
		if family.GetName() == "host_request_duration_ms" {
			for _, metric := range family.Metric {
				observed += metric.GetHistogram().GetSampleCount()
			}
		}
	}
	if observed != 2 {
		t.Fatal("acceptance and rejection were not both timed exactly once")
	}
}

func TestHostFailureLogUsesRequestTraceAndWireErrorCode(t *testing.T) {
	f := newFixture(t)
	var output bytes.Buffer
	f.service.logger = slog.New(slog.NewJSONHandler(&output, nil))
	f.store.rows["r1"] = store.Request{RequestID: "r1", HostID: "h1", DeviceID: "d1", TraceID: "operation-1", CreatedAt: f.now}
	result := &whalereinv1.HostResult{RequestId: "r1", Status: whalereinv1.OnlineStatus_rejected, ErrorCode: whalereinv1.ErrorCode_unauthorized, TraceId: "different-trace", RelayVersion: RelayEpoch}
	if err := f.service.HandleHostResult(context.Background(), "h1", result); err != nil {
		t.Fatal(err)
	}
	var record map[string]any
	if err := json.Unmarshal(bytes.TrimSpace(output.Bytes()), &record); err != nil {
		t.Fatal(err)
	}
	if record["trace_id"] != "operation-1" || record["code"] != "unauthorized" || record["layer"] != "host" || record["status"] != "rejected" || record["duration_ms"] == nil {
		t.Fatal("missing host failure evidence")
	}
}

func TestTransportBudgetAndVersionRejectionsStayDistinctFromHostOffline(t *testing.T) {
	for _, failure := range []error{broker.ErrPeerTransportUnsupported, broker.ErrMessageBudget} {
		f := newFixture(t)
		f.store.claimRows = []store.Request{{RequestID: "r", HostID: "h1", DeviceID: "d1", Kind: whalereinv1.RequestKind_REQUEST_KIND_QUERY, Status: whalereinv1.OnlineStatus_received, ExpiresAt: f.now.Add(time.Minute)}}
		f.broker.err = failure
		if err := f.service.ForwardDue(context.Background()); err != nil {
			t.Fatal(err)
		}
		if len(f.store.applyCalls) != 1 || f.store.applyCalls[0].Status != whalereinv1.OnlineStatus_rejected {
			t.Fatal("transport failure misreported")
		}
		expected := whalereinv1.ErrorCode_protocol_version_incompatible
		if failure == broker.ErrMessageBudget {
			expected = whalereinv1.ErrorCode_payload_too_large
		}
		if f.store.applyCalls[0].ErrorCode != expected {
			t.Fatal("transport error code lost")
		}
	}
}

func TestRelayEpochSurvivesDurableForwardingAndRejectsForeignEpochs(t *testing.T) {
	f := newFixture(t)
	request := validRequest(f.now, "epoch-cmd")
	row, _, err := f.service.Submit(context.Background(), input(request))
	if err != nil {
		t.Fatal(err)
	}
	if row.RelayVersion != RelayEpoch {
		t.Fatal("relay epoch lost on persistence")
	}
	f.store.claimRows = []store.Request{row}
	if err := f.service.ForwardDue(context.Background()); err != nil {
		t.Fatal(err)
	}
	if len(f.broker.published) == 0 || f.broker.published[len(f.broker.published)-1].RelayVersion != RelayEpoch {
		t.Fatal("relay epoch lost on replay")
	}
	for _, version := range []uint32{0, 2, 4} {
		request.RelayVersion = version
		if _, _, err := f.service.Submit(context.Background(), input(request)); !errors.Is(err, ErrProtocolVersion) {
			t.Fatalf("foreign epoch %d was admitted", version)
		}
	}
}

// TestSessionObservationAC26UnrelatedSubmitDoesNotRenew covers the
// other-submit branch: a new request never renews R, while an idempotent replay
// renews only its own exact row.
func TestSessionObservationAC26UnrelatedSubmitDoesNotRenew(t *testing.T) {
	f := newFixture(t)
	base := validRequest(f.now, "c1")
	first, created, err := f.service.Submit(context.Background(), input(base))
	if err != nil || !created {
		t.Fatalf("submit R: created=%v err=%v", created, err)
	}
	if first.ClientLeaseUntil == nil || first.ClientLeaseState != store.ClientLeaseLive {
		t.Fatalf("submit did not record an exact live lease: %+v", first)
	}
	if want := f.now.Add(clientPollLease); !first.ClientLeaseUntil.Equal(want) {
		t.Fatalf("initial lease = %v want %v", first.ClientLeaseUntil, want)
	}
	f.store.renewCalls = nil

	other := validRequest(f.now, "c2")
	other.RequestId = "req-other"
	second, created, err := f.service.Submit(context.Background(), input(other))
	if err != nil || !created {
		t.Fatalf("submit unrelated: created=%v err=%v", created, err)
	}
	if len(f.store.renewCalls) != 0 {
		t.Fatalf("unrelated submit renewed an existing request: %+v", f.store.renewCalls)
	}

	// A duplicate submit of the unrelated request is an exact replay and may
	// renew only that request.
	if _, _, err := f.service.Submit(context.Background(), input(other)); err != nil {
		t.Fatal(err)
	}
	if len(f.store.renewCalls) != 1 || f.store.renewCalls[0].requestID != second.RequestID || f.store.renewCalls[0].deviceID != "d1" {
		t.Fatalf("replay renewed the wrong scope: %+v", f.store.renewCalls)
	}
	if f.store.renewCalls[0].lease != clientPollLease {
		t.Fatalf("replay lease = %v", f.store.renewCalls[0].lease)
	}
}

// TestSessionObservationAC26TouchTargetsOnlyAddressedRequest covers
// other-GET/other-heartbeat and wrong-device rejection at the service seam.
func TestSessionObservationAC26TouchTargetsOnlyAddressedRequest(t *testing.T) {
	f := newFixture(t)
	f.store.renewResult = true
	renewed, err := f.service.Touch(context.Background(), "d1", "req-own")
	if err != nil || !renewed {
		t.Fatalf("own renewal = %v %v", renewed, err)
	}
	if len(f.store.renewCalls) != 1 || f.store.renewCalls[0].requestID != "req-own" || f.store.renewCalls[0].deviceID != "d1" {
		t.Fatalf("own renewal scope = %+v", f.store.renewCalls)
	}

	f.store.renewCalls = nil
	f.store.renewResult = false
	renewed, err = f.service.Touch(context.Background(), "d2", "req-own")
	if err != nil || renewed {
		t.Fatalf("wrong-device renewal = %v %v", renewed, err)
	}
	if len(f.store.renewCalls) != 1 || f.store.renewCalls[0].deviceID != "d2" || f.store.renewCalls[0].requestID != "req-own" {
		t.Fatalf("wrong-device scope = %+v", f.store.renewCalls)
	}
}

// TestSessionObservationAC27TerminalStatusStopsRenewal covers the
// terminal-wait/cancel/drop branch at the service seam: the store owns the
// status predicate, so Touch never reports success for a closed row.
func TestSessionObservationAC27TerminalStatusStopsRenewal(t *testing.T) {
	f := newFixture(t)
	f.store.renewResult = false
	for _, requestID := range []string{"req-succeeded", "req-failed", "req-cancelled"} {
		renewed, err := f.service.Touch(context.Background(), "d1", requestID)
		if err != nil || renewed {
			t.Fatalf("terminal %s renewed: %v %v", requestID, renewed, err)
		}
	}
}

// TestSessionObservationAC28LeaseExpiryPublishesExactCancellation covers the
// lease-expiry branch: the sweep publishes the expired exact identity.
func TestSessionObservationAC28LeaseExpiryPublishesExactCancellation(t *testing.T) {
	f := newFixture(t)
	row := store.Request{RequestID: "req-expired", HostID: "h1", DeviceID: "d1", CommandID: "cmd-1", Status: whalereinv1.OnlineStatus_received, ExpiresAt: f.now.Add(time.Minute)}
	f.store.expireRows = []store.Request{row}
	f.service.SweepClientLeases(context.Background())
	if len(f.broker.presence) != 1 || !strings.HasPrefix(f.broker.presence[0], "h1|") {
		t.Fatalf("presence = %v", f.broker.presence)
	}
	if len(f.store.cancelMarked) != 1 || f.store.cancelMarked[0] != "req-expired" {
		t.Fatalf("publish not recorded: %v", f.store.cancelMarked)
	}
	if len(f.store.attemptMarked) != 0 {
		t.Fatalf("successful publish recorded as an attempt: %v", f.store.attemptMarked)
	}
}

// TestSessionObservationAC30FirstPublishFailsThenRetries covers durable replay:
// a failed publish retains pending metadata, and a later sweep retries the same
// exact identity from the pending query.
func TestSessionObservationAC30FirstPublishFailsThenRetries(t *testing.T) {
	f := newFixture(t)
	row := store.Request{RequestID: "req-retry", HostID: "h1", DeviceID: "d1", CommandID: "cmd-r", Status: whalereinv1.OnlineStatus_received, ExpiresAt: f.now.Add(time.Minute)}
	f.store.expireRows = []store.Request{row}
	f.broker.err = errors.New("mqtt unavailable")
	f.service.SweepClientLeases(context.Background())
	if len(f.store.attemptMarked) != 1 || f.store.attemptMarked[0] != "req-retry" {
		t.Fatalf("failed publish did not retain pending metadata: %v", f.store.attemptMarked)
	}
	if len(f.store.cancelMarked) != 0 {
		t.Fatal("failed publish was recorded as published")
	}

	// Restart/pending replay: the same row is read from the pending query and
	// published once. A second publish cannot affect another request.
	f.broker.err = nil
	f.store.expireRows = nil
	f.store.pendingRows = []store.Request{row}
	f.store.attemptMarked = nil
	f.service.SweepClientLeases(context.Background())
	if len(f.broker.presence) != 2 || !strings.Contains(f.broker.presence[1], "req-retry") {
		t.Fatalf("replay did not publish the exact request: %v", f.broker.presence)
	}
	if len(f.store.cancelMarked) != 1 || f.store.cancelMarked[0] != "req-retry" {
		t.Fatalf("replay publish not recorded: %v", f.store.cancelMarked)
	}
}

// TestSessionObservationAC35PublishFailureIsNotNativeRelease covers the
// honest-fallback branch: a failed publish never claims release, and the row
// remains retryable.
func TestSessionObservationAC35PublishFailureIsNotNativeRelease(t *testing.T) {
	f := newFixture(t)
	f.broker.err = errors.New("mqtt down")
	f.store.expireRows = []store.Request{{RequestID: "req-fail", HostID: "h1", DeviceID: "d1", Status: whalereinv1.OnlineStatus_received, ExpiresAt: f.now.Add(time.Minute)}}
	f.service.SweepClientLeases(context.Background())
	if len(f.store.cancelMarked) != 0 {
		t.Fatal("failed publish marked native release")
	}
	if len(f.store.attemptMarked) != 1 {
		t.Fatal("failed publish did not increment its durable attempt")
	}
}

// TestSessionObservationAC33ExpiryTargetsOwningHostOnly covers the
// Host-scoped branch: each cancellation goes only to its row's owning host.
func TestSessionObservationAC33ExpiryTargetsOwningHostOnly(t *testing.T) {
	f := newFixture(t)
	f.presence.online["h2"] = true
	f.store.expireRows = []store.Request{
		{RequestID: "req-a", HostID: "h1", DeviceID: "d1", Status: whalereinv1.OnlineStatus_received, ExpiresAt: f.now.Add(time.Minute)},
		{RequestID: "req-b", HostID: "h2", DeviceID: "d1", Status: whalereinv1.OnlineStatus_received, ExpiresAt: f.now.Add(time.Minute)},
	}
	f.service.SweepClientLeases(context.Background())
	if len(f.broker.presence) != 2 {
		t.Fatalf("presence = %v", f.broker.presence)
	}
	if !strings.HasPrefix(f.broker.presence[0], "h1|") || !strings.Contains(f.broker.presence[0], "req-a") {
		t.Fatalf("first host scope = %q", f.broker.presence[0])
	}
	if !strings.HasPrefix(f.broker.presence[1], "h2|") || !strings.Contains(f.broker.presence[1], "req-b") {
		t.Fatalf("second host scope = %q", f.broker.presence[1])
	}
}

// TestSessionObservationAC34CancellationPayloadIsBoundedAndExact covers the
// no-content/exact-scope branch: only bounded identity/deadline metadata is
// published, never request, question or answer content.
func TestSessionObservationAC34CancellationPayloadIsBoundedAndExact(t *testing.T) {
	f := newFixture(t)
	row := store.Request{
		RequestID: "req-payload", HostID: "h1", DeviceID: "d1", CommandID: "cmd-payload",
		Status:      whalereinv1.OnlineStatus_received,
		ExpiresAt:   f.now.Add(time.Minute),
		RequestBody: []byte("question-and-answer-secret"),
		ResultBody:  []byte("answer-secret"),
	}
	f.store.expireRows = []store.Request{row}
	f.service.SweepClientLeases(context.Background())
	if len(f.broker.presence) != 1 {
		t.Fatalf("presence = %v", f.broker.presence)
	}
	payload := f.broker.presence[0]
	if !strings.HasPrefix(payload, "h1|") {
		t.Fatalf("topic scope = %q", payload)
	}
	body := strings.TrimPrefix(payload, "h1|")
	if len(body) > 512 {
		t.Fatalf("cancellation payload is unbounded: %d bytes", len(body))
	}
	for _, forbidden := range []string{"secret", "question", "answer", "request_body", "result_body"} {
		if strings.Contains(body, forbidden) {
			t.Fatalf("payload leaked %q: %s", forbidden, body)
		}
	}
	var decoded map[string]any
	if err := json.Unmarshal([]byte(body), &decoded); err != nil {
		t.Fatalf("payload is not JSON: %v", err)
	}
	for _, key := range []string{"event", "request_id", "host_id", "device_id", "command_id", "expires_at_unix_ms", "reason", "observed_at"} {
		if _, ok := decoded[key]; !ok {
			t.Fatalf("payload lost %q: %s", key, body)
		}
	}
	if decoded["request_id"] != "req-payload" || decoded["command_id"] != "cmd-payload" {
		t.Fatalf("payload identity = %s", body)
	}
}

// TestPresenceUnavailableKnownUnknownAndOnlineAreDistinct covers the three
// presence states a send can observe: the view is not reconciled yet (queue and
// retry), the host is explicitly offline (terminal host_offline), and the host
// is absent from the reconciled view (also offline, but reached through the
// "unknown" fake dimension rather than an explicit offline entry).
func TestPresenceUnavailableKnownUnknownAndOnlineAreDistinct(t *testing.T) {
	ctx := context.Background()
	f := newFixture(t)
	available, unavailable := true, false

	submit := func(id, command string) store.Request {
		t.Helper()
		request := validRequest(f.now, command)
		request.RequestId = id
		row, created, err := f.service.Submit(ctx, input(request))
		if err != nil || !created {
			t.Fatalf("%s row=%+v created=%v err=%v", id, row, created, err)
		}
		return row
	}

	// State 1: presence unavailable. The host lookup is deliberately false, but
	// it must not be read as a terminal offline answer.
	f.presence.available = &unavailable
	f.presence.observed = map[string]bool{"h1": true}
	f.presence.online["h1"] = false
	queued := submit("req-unavailable", "c-unavailable")
	if queued.Status != whalereinv1.OnlineStatus_received || queued.ForwardState != store.ForwardPending {
		t.Fatalf("unavailable presence closed the request: %+v", queued)
	}

	// State 2: presence reconciled but the host was never observed. That is a
	// known absence, not an unreconciled view, so it is terminally offline.
	f.presence.available = &available
	f.presence.observed = map[string]bool{}
	unknown := submit("req-unknown", "c-unknown")
	if unknown.Status != whalereinv1.OnlineStatus_host_offline || unknown.ForwardState != store.ForwardNone {
		t.Fatalf("unknown host was not treated as known offline: %+v", unknown)
	}

	// State 3: presence reconciled and the host is explicitly offline.
	f.presence.observed = map[string]bool{"h1": true}
	f.presence.online["h1"] = false
	offline := submit("req-offline", "c-offline")
	if offline.Status != whalereinv1.OnlineStatus_host_offline || offline.ForwardState != store.ForwardNone {
		t.Fatalf("known-offline host was not terminal: %+v", offline)
	}
	if queued.Status == unknown.Status || queued.Status == offline.Status {
		t.Fatal("unavailable and known-offline states collapsed")
	}

	// State 4: presence reconciled and the host online. The queued row is
	// forwarded, proving the retry path stays open after an unavailable view.
	f.presence.online["h1"] = true
	online := submit("req-online", "c-online")
	if online.Status != whalereinv1.OnlineStatus_received || online.ForwardState != store.ForwardPending {
		t.Fatalf("online host was not left forwardable: %+v", online)
	}
	f.store.claimRows = []store.Request{online}
	if err := f.service.ForwardDue(ctx); err != nil {
		t.Fatal(err)
	}
	if len(f.broker.published) != 1 || len(f.store.published) != 1 {
		t.Fatalf("online request not forwarded: broker=%d store=%d", len(f.broker.published), len(f.store.published))
	}
}

func TestSessionObservationAC27InitialLeaseIsBoundedByDeadline(t *testing.T) {
	f := newFixture(t)
	request := validRequest(f.now, "c-short")
	request.ExpiresAtUnixMs = f.now.Add(2 * time.Second).UnixMilli()
	row, created, err := f.service.Submit(context.Background(), input(request))
	if err != nil || !created {
		t.Fatalf("submit = %v %v", created, err)
	}
	if row.ClientLeaseUntil == nil || !row.ClientLeaseUntil.Equal(row.ExpiresAt) {
		t.Fatalf("lease %v not bounded by request deadline %v", row.ClientLeaseUntil, row.ExpiresAt)
	}
}

func TestSessionObservationAC29TouchReportsRenewalFailure(t *testing.T) {
	f := newFixture(t)
	f.store.renewErr = errors.New("database unavailable")
	if renewed, err := f.service.Touch(context.Background(), "d1", "r1"); err == nil || renewed {
		t.Fatalf("renewal failure = %v %v", renewed, err)
	}
}

func TestSessionObservationAC30SweepStoreFailuresRetainWork(t *testing.T) {
	f := newFixture(t)
	f.store.expireErr = errors.New("database unavailable")
	f.service.SweepClientLeases(context.Background())
	if len(f.broker.presence) != 0 {
		t.Fatal("expiry failure published a notice")
	}

	f.store.expireErr = nil
	f.store.pendingErr = errors.New("database unavailable")
	f.service.SweepClientLeases(context.Background())
	if len(f.broker.presence) != 0 {
		t.Fatal("pending query failure published a notice")
	}
}

func TestSessionObservationAC30OfflineHostRetainsPendingCancellation(t *testing.T) {
	f := newFixture(t)
	f.store.expireRows = []store.Request{{RequestID: "r1", HostID: "h1", DeviceID: "d1", Status: whalereinv1.OnlineStatus_received, ExpiresAt: f.now.Add(time.Minute)}}
	f.presence.online["h1"] = false
	f.service.SweepClientLeases(context.Background())
	if len(f.broker.presence) != 0 || len(f.store.cancelMarked) != 0 || len(f.store.attemptMarked) != 0 {
		t.Fatalf("offline host was treated as delivered: %v %v %v", f.broker.presence, f.store.cancelMarked, f.store.attemptMarked)
	}

	unavailable := false
	f.presence.available = &unavailable
	f.presence.online["h1"] = true
	f.service.SweepClientLeases(context.Background())
	if len(f.broker.presence) != 0 || len(f.store.cancelMarked) != 0 {
		t.Fatal("unavailable presence published a notice")
	}
}

func TestSessionObservationAC35BookkeepingFailuresDoNotClaimRelease(t *testing.T) {
	f := newFixture(t)
	f.store.expireRows = []store.Request{{RequestID: "r1", HostID: "h1", DeviceID: "d1", Status: whalereinv1.OnlineStatus_received, ExpiresAt: f.now.Add(time.Minute)}}
	f.store.cancelMarkErr = errors.New("database unavailable")
	f.service.SweepClientLeases(context.Background())
	if len(f.store.attemptMarked) != 0 {
		t.Fatal("a successful publish was counted as a failed attempt")
	}

	f.store.cancelMarkErr = nil
	f.broker.err = errors.New("mqtt unavailable")
	f.store.attemptErr = errors.New("database unavailable")
	f.service.SweepClientLeases(context.Background())
	if len(f.store.cancelMarked) != 0 {
		t.Fatal("failed bookkeeping claimed release")
	}
}

func TestSessionObservationAC30DuplicateSubmitRenewalFailure(t *testing.T) {
	f := newFixture(t)
	request := validRequest(f.now, "c-replay")
	if _, _, err := f.service.Submit(context.Background(), input(request)); err != nil {
		t.Fatal(err)
	}
	f.store.renewErr = errors.New("database unavailable")
	if _, _, err := f.service.Submit(context.Background(), input(request)); err == nil {
		t.Fatal("duplicate submit ignored renewal failure")
	}
}

func TestNewRejectsMissingDependencies(t *testing.T) {
	broker := &fakeBroker{}
	presence := &fakePresence{online: map[string]bool{}}
	for _, tc := range []struct {
		name string
		deps Dependencies
	}{
		{"store", Dependencies{Broker: broker, Presence: presence}},
		{"broker", Dependencies{Store: newFakeStore(), Presence: presence}},
		{"presence", Dependencies{Store: newFakeStore(), Broker: broker}},
	} {
		if _, err := New(tc.deps); err == nil {
			t.Fatalf("missing %s accepted", tc.name)
		}
	}
}

func TestResultLimitConversionsStayBounded(t *testing.T) {
	cases := []struct {
		raw, encoded, decoded int64
	}{
		{0, resultEnvelopeOverheadBytes, 0},
		{1, resultEnvelopeOverheadBytes + 4, 3},
		{3, resultEnvelopeOverheadBytes + 4, 3},
		{3000, resultEnvelopeOverheadBytes + 4000, 3000},
		{16 << 20, resultEnvelopeOverheadBytes + ((16<<20+2)/3)*4, 16<<20 + 2},
	}
	for _, tc := range cases {
		if got := EncodedResultLimit(tc.raw); got != tc.encoded {
			t.Fatalf("EncodedResultLimit(%d) = %d, want %d", tc.raw, got, tc.encoded)
		}
		if got := ResultValueLimit(tc.encoded); got != tc.decoded {
			t.Fatalf("ResultValueLimit(%d) = %d, want %d", tc.encoded, got, tc.decoded)
		}
	}
	if ResultValueLimit(-1) != 0 || ResultValueLimit(0) != 0 {
		t.Fatal("nonpositive encoded limits must allow no ciphertext")
	}
}

func TestSubmitCapturesDurableStampAndIgnoresCallerGeneration(t *testing.T) {
	f := newFixture(t)
	request := validRequest(f.now, "c1")
	request.Transport = &whalereinv1.RequestTransportMetadata{
		ResultMaxBytes:       1 << 20,
		GrantGeneration:      999,
		RevocationGeneration: 999,
	}
	row, created, err := f.service.Submit(context.Background(), input(request))
	if err != nil || !created {
		t.Fatalf("row=%+v created=%v err=%v", row, created, err)
	}
	if row.Stamp == nil {
		t.Fatal("admission did not capture a grant stamp")
	}
	if row.Stamp.GrantGeneration != 7 || row.Stamp.RevocationGeneration != 3 || row.Stamp.DeviceRevocationGeneration != 2 {
		t.Fatalf("stamp did not come from durable grant state: %+v", row.Stamp)
	}
	wantLimit := ResultValueLimit(1 << 20)
	if row.Stamp.ResultLimitBytes != wantLimit {
		t.Fatalf("result limit = %d, want %d", row.Stamp.ResultLimitBytes, wantLimit)
	}
	if len(f.grants.calls) != 1 || f.grants.calls[0] != "h1|d1" {
		t.Fatalf("grant reads = %v", f.grants.calls)
	}
	f.store.claimRows = []store.Request{row}
	if err := f.service.ForwardDue(context.Background()); err != nil {
		t.Fatal(err)
	}
	if len(f.broker.published) != 1 {
		t.Fatalf("published = %d", len(f.broker.published))
	}
	forwarded := f.broker.published[0].GetTransport()
	if forwarded.GetGrantGeneration() != 7 || forwarded.GetRevocationGeneration() != 3 {
		t.Fatalf("forwarded metadata did not carry durable revisions: %+v", forwarded)
	}
	if forwarded.GetResultMaxBytes() != uint64(EncodedResultLimit(wantLimit)) {
		t.Fatalf("forwarded result bound = %d", forwarded.GetResultMaxBytes())
	}

	f.grants.grants["h1|d1"] = store.AdmissionGrant{HostID: "h1", DeviceID: "d1", Active: false, GrantGeneration: 7}
	if _, _, err := f.service.Submit(context.Background(), input(validRequest(f.now, "c2"))); !errors.Is(err, ErrForbidden) {
		t.Fatalf("inactive grant = %v", err)
	}
	f.grants.err = errors.New("db down")
	if _, _, err := f.service.Submit(context.Background(), input(validRequest(f.now, "c3"))); err == nil {
		t.Fatal("expected durable grant read failure")
	}
	delete(f.grants.grants, "h1|d1")
	f.grants.err = nil
	if _, _, err := f.service.Submit(context.Background(), input(validRequest(f.now, "c4"))); !errors.Is(err, ErrForbidden) {
		t.Fatalf("missing grant = %v", err)
	}
}

func TestSubmitReplayDoesNotRefreshStoredStamp(t *testing.T) {
	f := newFixture(t)
	existing := store.Request{
		RequestID: "req-old", HostID: "h1", DeviceID: "d1", CommandID: "c1",
		Status: whalereinv1.OnlineStatus_succeeded, ExpiresAt: f.now.Add(time.Hour),
		Stamp: &store.GrantStamp{GrantGeneration: 2, RevocationGeneration: 1, DeviceRevocationGeneration: 1, ResultLimitBytes: 10},
	}
	f.store.createOverride = func(store.Request) (store.CreateOutcome, error) {
		return store.CreateOutcome{Request: existing}, nil
	}
	f.grants.grants["h1|d1"] = store.AdmissionGrant{HostID: "h1", DeviceID: "d1", Active: true, GrantGeneration: 9, RevocationGeneration: 9, DeviceRevocationGeneration: 9}
	request := validRequest(f.now, "c1")
	request.RequestId = "req-new"
	request.Transport = &whalereinv1.RequestTransportMetadata{ResultMaxBytes: 1 << 20}
	row, created, err := f.service.Submit(context.Background(), input(request))
	if err != nil || created || row.RequestID != "req-old" {
		t.Fatalf("row=%+v created=%v err=%v", row, created, err)
	}
	if row.Stamp == nil || row.Stamp.GrantGeneration != 2 || row.Stamp.ResultLimitBytes != 10 {
		t.Fatalf("replay refreshed the stored stamp: %+v", row.Stamp)
	}
	if len(f.broker.published) != 0 {
		t.Fatal("replay republished the request")
	}
}

func TestReleaseAppliesFenceByKind(t *testing.T) {
	baseRow := func(kind whalereinv1.RequestKind) store.Request {
		return store.Request{
			RequestID: "r1", HostID: "h1", DeviceID: "d1", CommandID: "c1", PayloadDigest: []byte("digest"),
			Kind: kind, Status: whalereinv1.OnlineStatus_succeeded, ResultBody: []byte("ciphertext"),
			ErrorCode: whalereinv1.ErrorCode_internal, ErrorMessage: "host failure",
			ExpiresAt: time.Date(2026, 11, 9, 12, 30, 0, 0, time.UTC),
			Stamp:     &store.GrantStamp{GrantGeneration: 7, RevocationGeneration: 3, DeviceRevocationGeneration: 2, ResultLimitBytes: 1 << 20},
		}
	}
	allowed := func() store.ReleaseOutcome {
		return store.ReleaseOutcome{
			Request:      baseRow(whalereinv1.RequestKind_REQUEST_KIND_QUERY),
			SessionValid: true, StampPresent: true, GrantActive: true, StampCurrent: true,
			WithinResultBound: true, WithinDelivery: true,
		}
	}
	cases := []struct {
		name     string
		mutate   func(*store.ReleaseOutcome)
		err      error
		wantBody string
		twice    bool
	}{
		{"query current grant", func(*store.ReleaseOutcome) {}, nil, "ciphertext", false},
		{"stored before revoke first get", func(o *store.ReleaseOutcome) { o.GrantActive = false; o.StampCurrent = false }, ErrReleaseFenced, "", false},
		{"repeat get after revoke", func(o *store.ReleaseOutcome) { o.GrantActive = false; o.StampCurrent = false }, ErrReleaseFenced, "", true},
		{"old result after regrant", func(o *store.ReleaseOutcome) { o.StampCurrent = false }, ErrReleaseFenced, "", false},
		{"missing stamp", func(o *store.ReleaseOutcome) { o.StampPresent = false; o.StampCurrent = false }, ErrReleaseFenced, "", false},
		{"body cleared", func(o *store.ReleaseOutcome) { o.BodiesCleared = true }, ErrReleaseFenced, "", false},
		{"oversized under stored bound", func(o *store.ReleaseOutcome) { o.WithinResultBound = false }, ErrReleaseFenced, "", false},
		{"lowered delivery cap", func(o *store.ReleaseOutcome) { o.WithinDelivery = false }, ErrReleaseFenced, "", false},
		{"foreign device", func(o *store.ReleaseOutcome) { o.Request = store.Request{} }, ErrNotFound, "", false},
		{"invalid session", func(o *store.ReleaseOutcome) { o.SessionValid = false }, ErrForbidden, "", false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			f := newFixture(t)
			outcome := allowed()
			tc.mutate(&outcome)
			f.store.releaseOutcome = outcome
			if tc.name == "foreign device" {
				f.store.releaseErr = store.ErrNotFound
			}
			row, err := f.service.Release(context.Background(), "d1", "", "r1", 1<<20)
			if !errors.Is(err, tc.err) {
				t.Fatalf("err = %v, want %v", err, tc.err)
			}
			if tc.err != nil && len(row.ResultBody) != 0 {
				t.Fatal("fenced response carried ciphertext")
			}
			if string(row.ResultBody) != tc.wantBody {
				t.Fatalf("body = %q, want %q", row.ResultBody, tc.wantBody)
			}
			if tc.twice {
				if _, err := f.service.Release(context.Background(), "d1", "", "r1", 1<<20); !errors.Is(err, ErrReleaseFenced) {
					t.Fatalf("repeat release = %v", err)
				}
			}
		})
	}

	t.Run("command metadata-only when fenced", func(t *testing.T) {
		f := newFixture(t)
		outcome := allowed()
		outcome.Request = baseRow(whalereinv1.RequestKind_REQUEST_KIND_COMMAND)
		outcome.StampCurrent = false
		outcome.GrantActive = false
		f.store.releaseOutcome = outcome
		row, err := f.service.Release(context.Background(), "d1", "", "r1", 1<<20)
		if err != nil {
			t.Fatal(err)
		}
		if len(row.ResultBody) != 0 {
			t.Fatal("fenced command response carried ciphertext")
		}
		if row.Status != whalereinv1.OnlineStatus_succeeded || row.ErrorCode != whalereinv1.ErrorCode_internal ||
			row.CommandID != "c1" || !bytes.Equal(row.PayloadDigest, []byte("digest")) {
			t.Fatalf("command metadata was not preserved: %+v", row)
		}
	})

	t.Run("access-authored result keeps existing release", func(t *testing.T) {
		f := newFixture(t)
		outcome := allowed()
		outcome.Request = baseRow(whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE)
		outcome.StampPresent = false
		outcome.StampCurrent = false
		outcome.GrantActive = false
		f.store.releaseOutcome = outcome
		row, err := f.service.Release(context.Background(), "d1", "", "r1", 1<<20)
		if err != nil || string(row.ResultBody) != "ciphertext" {
			t.Fatalf("access-authored release changed: %+v, %v", row, err)
		}
	})

	f := newFixture(t)
	outcome := allowed()
	outcome.Request.Status = whalereinv1.OnlineStatus_received
	outcome.Request.ExpiresAt = f.now.Add(-time.Second)
	f.store.releaseOutcome = outcome
	if _, err := f.service.Release(context.Background(), "d1", "", "r1", 1<<20); !errors.Is(err, ErrExpired) {
		t.Fatalf("expired pending release = %v", err)
	}

	f = newFixture(t)
	outcome = allowed()
	outcome.Request.ResultBody = nil
	outcome.StampPresent = false
	outcome.StampCurrent = false
	f.store.releaseOutcome = outcome
	if _, err := f.service.Release(context.Background(), "d1", "", "r1", 1<<20); !errors.Is(err, ErrReleaseFenced) {
		t.Fatalf("missing stamp query without a body = %v", err)
	}
	if testutil.ToFloat64(f.service.metrics.ReleaseFenced) == 0 {
		t.Fatal("release fence was not counted")
	}
}

func TestHandleHostResultCommitsOutcomeWhenBodyRefused(t *testing.T) {
	f := newFixture(t)
	f.store.rows["r1"] = store.Request{
		RequestID: "r1", HostID: "h1", DeviceID: "d1", Kind: whalereinv1.RequestKind_REQUEST_KIND_QUERY,
		Status: whalereinv1.OnlineStatus_received,
		Stamp:  &store.GrantStamp{GrantGeneration: 1, RevocationGeneration: 1, DeviceRevocationGeneration: 1, ResultLimitBytes: 4},
	}
	result := &whalereinv1.HostResult{RequestId: "r1", Status: whalereinv1.OnlineStatus_succeeded, ResultBody: []byte("12345"), ErrorCode: whalereinv1.ErrorCode_internal, RelayVersion: RelayEpoch}
	if err := f.service.HandleHostResult(context.Background(), "h1", result); err != nil {
		t.Fatalf("refused body discarded the outcome: %v", err)
	}
	if len(f.store.applyCalls) != 1 {
		t.Fatalf("apply calls = %d", len(f.store.applyCalls))
	}
	update := f.store.applyCalls[0]
	if update.Status != whalereinv1.OnlineStatus_succeeded || update.ErrorCode != whalereinv1.ErrorCode_internal {
		t.Fatalf("outcome changed with refused body: %+v", update)
	}
	if len(update.ResultBody) != 0 {
		t.Fatalf("refused body reached durable storage: %d bytes", len(update.ResultBody))
	}
	if testutil.ToFloat64(f.service.metrics.ResultBoundRejected) != 1 {
		t.Fatal("result bound rejection was not counted")
	}

	// A row retained from before the stamp still obeys the current Access cap.
	f = newFixture(t)
	f.service.cfg.MaxResultBytes = 4
	f.store.rows["r1"] = store.Request{RequestID: "r1", HostID: "h1", DeviceID: "d1", Status: whalereinv1.OnlineStatus_received}
	if err := f.service.HandleHostResult(context.Background(), "h1", result); err != nil {
		t.Fatalf("legacy refused body discarded the outcome: %v", err)
	}
	if len(f.store.applyCalls) != 1 || len(f.store.applyCalls[0].ResultBody) != 0 {
		t.Fatal("legacy refused body reached durable storage")
	}
}

func TestSubmitClampsDeclaredResultBoundToAccessBudgets(t *testing.T) {
	f := newFixture(t)
	f.service.cfg.MaxResultBytes = 1000
	request := validRequest(f.now, "c1")
	request.Transport = &whalereinv1.RequestTransportMetadata{ResultMaxBytes: 1 << 30}
	row, _, err := f.service.Submit(context.Background(), input(request))
	if err != nil || row.Stamp == nil || row.Stamp.ResultLimitBytes != 1000 {
		t.Fatalf("retained cap was not enforced: %+v, %v", row.Stamp, err)
	}

	f = newFixture(t)
	f.service.cfg.MaxResultBytes = 1 << 20
	f.service.cfg.MaxDeliveryBytes = resultEnvelopeOverheadBytes + 4000 // room for 3000 raw bytes
	request = validRequest(f.now, "c2")
	request.Transport = &whalereinv1.RequestTransportMetadata{ResultMaxBytes: 1 << 30}
	row, _, err = f.service.Submit(context.Background(), input(request))
	if err != nil || row.Stamp == nil || row.Stamp.ResultLimitBytes != 3000 {
		t.Fatalf("delivery budget was not enforced: %+v, %v", row.Stamp, err)
	}

	f = newFixture(t)
	request = validRequest(f.now, "c3")
	row, _, err = f.service.Submit(context.Background(), input(request))
	if err != nil || row.Stamp == nil || row.Stamp.ResultLimitBytes != f.service.cfg.MaxResultBytes {
		t.Fatalf("unset declaration did not use the Access cap: %+v, %v", row.Stamp, err)
	}
}
