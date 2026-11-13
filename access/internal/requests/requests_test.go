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

type fakeBroker struct {
	published []*whalereinv1.HostRequest
	hosts     []string
	presence  []string
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

type fakePresence struct{ online map[string]bool }

func (f *fakePresence) IsHostOnline(_ context.Context, hostID string) bool { return f.online[hostID] }

type fixture struct {
	service  *Service
	store    *fakeStore
	broker   *fakeBroker
	presence *fakePresence
	now      time.Time
}

func newFixture(t *testing.T) *fixture {
	t.Helper()
	fs := newFakeStore()
	broker := &fakeBroker{}
	presence := &fakePresence{online: map[string]bool{"h1": true}}
	now := time.Date(2026, 11, 9, 12, 0, 0, 0, time.UTC)
	ref := &fixture{store: fs, broker: broker, presence: presence, now: now}
	service, err := New(Dependencies{
		Store: fs, Broker: broker, Presence: presence,
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
	f.store.rows["r1"] = store.Request{RequestID: "r1", DeviceID: "d1"}
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
	result = &whalereinv1.HostResult{RequestId: "r1", Status: whalereinv1.OnlineStatus_host_accepted}
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
	service, err := New(Dependencies{Store: f.store, Broker: f.broker, Presence: f.presence, Config: f.service.cfg, Logger: f.service.logger, Masker: f.service.masker, Now: func() time.Time { return f.now }})
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
	result := &whalereinv1.HostResult{RequestId: "r1", Status: whalereinv1.OnlineStatus_rejected, ErrorCode: whalereinv1.ErrorCode_unauthorized, TraceId: "different-trace"}
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

func TestReceiverCapabilitySurvivesDurableForwardingAndRejectsUnknownVersions(t *testing.T) {
	f := newFixture(t)
	request := validRequest(f.now, "capability-cmd")
	request.ClientTransportVersion = 2
	row, _, err := f.service.Submit(context.Background(), input(request))
	if err != nil {
		t.Fatal(err)
	}
	if row.ClientTransportVersion != 2 {
		t.Fatal("receiver capability lost on persistence")
	}
	f.store.claimRows = []store.Request{row}
	if err := f.service.ForwardDue(context.Background()); err != nil {
		t.Fatal(err)
	}
	if len(f.broker.published) == 0 || f.broker.published[len(f.broker.published)-1].ClientTransportVersion != 2 {
		t.Fatal("receiver capability lost on replay")
	}
	request.ClientTransportVersion = ^uint32(0)
	if _, _, err := f.service.Submit(context.Background(), input(request)); !errors.Is(err, ErrProtocolVersion) {
		t.Fatal("unknown capability poisoned durable queue")
	}
}

func TestClientPollLeaseReleasesStaleNonTerminalRequest(t *testing.T) {
	f := newFixture(t)
	row := store.Request{RequestID: "req-lease", HostID: "h1", DeviceID: "d1", Status: whalereinv1.OnlineStatus_received}
	f.store.rows[row.RequestID] = row
	f.service.recordClientClaim(row)
	f.now = f.now.Add(9 * time.Second)
	f.service.Touch("d1", row.RequestID, whalereinv1.OnlineStatus_received)
	f.now = f.now.Add(2 * time.Second)
	f.service.SweepClientClaims(context.Background())
	if len(f.broker.presence) != 0 {
		t.Fatalf("renewed claim swept early: %v", f.broker.presence)
	}
	f.now = f.now.Add(11 * time.Second)
	f.service.SweepClientClaims(context.Background())
	if len(f.broker.presence) != 1 {
		t.Fatalf("stale claim not published: %v", f.broker.presence)
	}
	if !strings.Contains(f.broker.presence[0], "request_stale") || !strings.Contains(f.broker.presence[0], "req-lease") {
		t.Fatalf("presence payload = %q", f.broker.presence[0])
	}
}

func TestClientPollLeaseIgnoresTerminalRequest(t *testing.T) {
	f := newFixture(t)
	row := store.Request{RequestID: "req-done", HostID: "h1", DeviceID: "d1", Status: whalereinv1.OnlineStatus_succeeded}
	f.store.rows[row.RequestID] = row
	f.service.recordClientClaim(row)
	f.now = f.now.Add(11 * time.Second)
	f.service.SweepClientClaims(context.Background())
	if len(f.broker.presence) != 0 {
		t.Fatalf("terminal claim published: %v", f.broker.presence)
	}
}

func TestClientPollTouchIgnoresUnknownWrongDeviceAndTerminal(t *testing.T) {
	f := newFixture(t)
	row := store.Request{RequestID: "req-touch", HostID: "h1", DeviceID: "d1", Status: whalereinv1.OnlineStatus_received}
	f.store.rows[row.RequestID] = row
	f.service.recordClientClaim(row)
	f.service.Touch("other", row.RequestID, whalereinv1.OnlineStatus_received)
	f.service.Touch("d1", "missing", whalereinv1.OnlineStatus_received)
	f.now = f.now.Add(9 * time.Second)
	f.service.SweepClientClaims(context.Background())
	if len(f.broker.presence) != 0 {
		t.Fatalf("ignored touch renewed or swept: %v", f.broker.presence)
	}
	f.service.Touch("d1", row.RequestID, whalereinv1.OnlineStatus_succeeded)
	f.now = f.now.Add(11 * time.Second)
	f.service.SweepClientClaims(context.Background())
	if len(f.broker.presence) != 0 {
		t.Fatalf("terminal touch not removed: %v", f.broker.presence)
	}
}

func TestClientPollSweepPublishFailureIsCounted(t *testing.T) {
	f := newFixture(t)
	f.broker.err = errors.New("offline")
	row := store.Request{RequestID: "req-fail", HostID: "h1", DeviceID: "d1", Status: whalereinv1.OnlineStatus_received}
	f.store.rows[row.RequestID] = row
	f.service.recordClientClaim(row)
	f.now = f.now.Add(11 * time.Second)
	f.service.SweepClientClaims(context.Background())
	if len(f.broker.presence) != 1 {
		t.Fatalf("publish attempt missing: %v", f.broker.presence)
	}
}
