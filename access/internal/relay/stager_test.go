package relay

import (
	"bytes"
	"context"
	"crypto/sha256"
	"errors"
	"io"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/broker"
	"github.com/whalerein/whalerein/access/internal/payload"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

type stagerStore struct {
	request store.Request
	grant   store.AdmissionGrant
	object  store.PayloadObject
	stale   []store.PayloadObject
	err     error
}

func (s *stagerStore) GetRequest(context.Context, string) (store.Request, error) {
	return s.request, s.err
}
func (s *stagerStore) AdmissionGrantFor(context.Context, string, string) (store.AdmissionGrant, error) {
	return s.grant, s.err
}
func (s *stagerStore) GetPayload(context.Context, string) (store.PayloadObject, error) {
	return s.object, s.err
}
func (s *stagerStore) StalePayloadScopes(context.Context, int) ([]store.PayloadObject, error) {
	return s.stale, s.err
}

type stagerPayloads struct {
	reserveErr error
	writeErr   error
	finishErr  error
	cancelErr  error
	checkErr   error
	inputs     []payload.ReserveInput
	written    []byte
	cancelled  []payload.Ref
}

func (p *stagerPayloads) Reserve(_ context.Context, input payload.ReserveInput) (payload.Object, error) {
	p.inputs = append(p.inputs, input)
	if p.reserveErr != nil {
		return payload.Object{}, p.reserveErr
	}
	return payload.Object{
		ObjectID: "0123456789abcdef0123456789abcdef", OwnerID: "node-1",
		HostID: input.HostID, DeviceID: input.DeviceID, AppSession: input.AppSession,
		ChannelID: input.ChannelID, RequestID: input.RequestID,
		Purpose: input.Purpose, Length: input.Length, Reserved: input.Length,
		Digest: input.Digest, ExpiresAt: time.UnixMilli(1_700_000_100_000),
	}, nil
}

func (p *stagerPayloads) Write(_ context.Context, _ payload.Ref, body io.Reader) error {
	data, _ := io.ReadAll(body)
	p.written = data
	return p.writeErr
}

func (p *stagerPayloads) Finish(_ context.Context, _ payload.Ref) (payload.Object, error) {
	if p.finishErr != nil {
		return payload.Object{}, p.finishErr
	}
	input := p.inputs[len(p.inputs)-1]
	return payload.Object{
		ObjectID: "0123456789abcdef0123456789abcdef", OwnerID: "node-1",
		Length: input.Length, Digest: input.Digest, ExpiresAt: time.UnixMilli(1_700_000_100_000),
	}, nil
}

func (p *stagerPayloads) Cancel(_ context.Context, ref payload.Ref) error {
	p.cancelled = append(p.cancelled, ref)
	return p.cancelErr
}

func (p *stagerPayloads) Check(context.Context, payload.Ref) (payload.Object, error) {
	return payload.Object{}, p.checkErr
}

func stagerFixture(t *testing.T) (*Stager, *stagerStore, *stagerPayloads) {
	t.Helper()
	request := store.Request{
		RequestID: "r1", HostID: "h1", DeviceID: "d1", DeviceSessionID: "s1",
		ChannelID: "c1", ExpiresAt: time.UnixMilli(1_700_000_200_000),
		Stamp: &store.GrantStamp{GrantGeneration: 7, RevocationGeneration: 3, DeviceRevocationGeneration: 2},
	}
	state := &stagerStore{request: request, grant: store.AdmissionGrant{
		HostID: "h1", DeviceID: "d1", Active: true,
		GrantGeneration: 7, RevocationGeneration: 3, DeviceRevocationGeneration: 2,
	}}
	payloads := &stagerPayloads{}
	service, err := NewStager(state, payloads, StagerConfig{
		Enabled: true, NodeID: "node-1", Retention: time.Hour,
	})
	if err != nil {
		t.Fatal(err)
	}
	service.now = func() time.Time { return time.UnixMilli(1_700_000_000_000) }
	return service, state, payloads
}

func TestStagerStagesRequestWithDurableScope(t *testing.T) {
	service, _, payloads := stagerFixture(t)
	ciphertext := []byte("sealed command")
	reference, err := service.Stage(context.Background(), broker.StageRequest{
		HostID: "h1", DeviceID: "d1", ChannelID: "c1", RequestID: "r1", MessageID: "r1",
		Purpose: whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_REQUEST, Ciphertext: ciphertext,
	})
	if err != nil {
		t.Fatal(err)
	}
	if reference.GetOwnerNodeId() != "node-1" || reference.GetCiphertextBytes() != uint64(len(ciphertext)) {
		t.Fatalf("reference = %+v", reference)
	}
	digest := sha256.Sum256(ciphertext)
	if !bytes.Equal(reference.GetSha256(), digest[:]) {
		t.Fatal("reference digest mismatch")
	}
	if len(payloads.inputs) != 1 {
		t.Fatal("reserve not called")
	}
	input := payloads.inputs[0]
	if input.AppSession != "s1" || input.RequestID != "r1" || input.ChannelID != "c1" ||
		input.GrantGeneration == nil || *input.GrantGeneration != 7 || !input.ParentDeadline.Equal(storeRequestDeadline()) {
		t.Fatalf("staged scope = %+v", input)
	}
	if !bytes.Equal(payloads.written, ciphertext) {
		t.Fatal("written ciphertext changed")
	}
}

func storeRequestDeadline() time.Time { return time.UnixMilli(1_700_000_200_000) }

func TestStagerFailsExplicitlyWhenDisabledOrRefused(t *testing.T) {
	disabled, _ := NewStager(nil, nil, StagerConfig{})
	if _, err := disabled.Stage(context.Background(), broker.StageRequest{Ciphertext: []byte("x")}); !errors.Is(err, broker.ErrStagingUnavailable) {
		t.Fatalf("disabled = %v", err)
	}
	service, _, payloads := stagerFixture(t)
	payloads.reserveErr = payload.ErrTooLarge
	_, err := service.Stage(context.Background(), broker.StageRequest{
		HostID: "h1", DeviceID: "d1", ChannelID: "c1", RequestID: "r1",
		Purpose: whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_REQUEST, Ciphertext: []byte("x"),
	})
	if !errors.Is(err, broker.ErrMessageBudget) {
		t.Fatalf("refused = %v", err)
	}
	payloads.reserveErr = nil
	payloads.writeErr = errors.New("disk full")
	_, err = service.Stage(context.Background(), broker.StageRequest{
		HostID: "h1", DeviceID: "d1", ChannelID: "c1", RequestID: "r1",
		Purpose: whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_RESULT, Ciphertext: []byte("x"),
	})
	if err == nil || !errors.Is(err, broker.ErrPeerTransportUnsupported) {
		t.Fatalf("write failure = %v", err)
	}
	if len(payloads.cancelled) != 1 {
		t.Fatal("failed staging was not cancelled")
	}
}

func TestStagerCheckPayloadBindsScopeAndGrantRevision(t *testing.T) {
	service, state, _ := stagerFixture(t)
	ciphertext := []byte("sealed result")
	digest := sha256.Sum256(ciphertext)
	reference := &whalereinv1.PayloadReference{
		ObjectId: "0123456789abcdef0123456789abcdef", OwnerNodeId: "node-1",
		CiphertextBytes: uint64(len(ciphertext)), Sha256: digest[:],
		Purpose: whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_RESULT,
	}
	grant, revocation, deviceRevocation := int64(7), int64(3), int64(2)
	state.object = store.PayloadObject{
		ObjectID: reference.ObjectId, OwnerID: "node-1", HostID: "h1", DeviceID: "d1",
		AppSession: "s1", ChannelID: "c1", RequestID: "r1",
		Purpose: whalereinv1.PayloadPurpose_name[int32(whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_RESULT)],
		State:   store.PayloadReady, Length: int64(len(ciphertext)), Digest: digest[:],
		ExpiresAt:       time.UnixMilli(1_700_000_100_000),
		GrantGeneration: &grant, RevocationGeneration: &revocation, DeviceRevocationGeneration: &deviceRevocation,
	}
	scope := store.PayloadCheckScope{
		HostID: "h1", DeviceID: "d1", AppSession: "s1", ChannelID: "c1", RequestID: "r1",
		Purpose: whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_RESULT,
	}
	if err := service.CheckPayload(context.Background(), reference, scope); err != nil {
		t.Fatalf("valid reference = %v", err)
	}
	scope.ChannelID = "other"
	if err := service.CheckPayload(context.Background(), reference, scope); !errors.Is(err, payload.ErrForbidden) {
		t.Fatalf("foreign channel = %v", err)
	}
	scope.ChannelID = "c1"
	state.grant.RevocationGeneration = 4
	if err := service.CheckPayload(context.Background(), reference, scope); !errors.Is(err, payload.ErrForbidden) {
		t.Fatalf("re-granted reference = %v", err)
	}
}

func TestStagerSweepCancelsRevokedScopes(t *testing.T) {
	service, state, payloads := stagerFixture(t)
	state.stale = []store.PayloadObject{{
		ObjectID: "0123456789abcdef0123456789abcdef", HostID: "h1", DeviceID: "d1",
		AppSession: "s1", ChannelID: "c1", Purpose: "PAYLOAD_PURPOSE_RESULT",
	}}
	cancelled, err := service.SweepRevoked(context.Background())
	if err != nil || cancelled != 1 {
		t.Fatalf("sweep = %d err=%v", cancelled, err)
	}
	if len(payloads.cancelled) != 1 || payloads.cancelled[0].AppSession != "s1" {
		t.Fatal("revoked scope was not cancelled with its exact session")
	}
}

func requestStage(requestID string) broker.StageRequest {
	return broker.StageRequest{
		HostID: "h1", DeviceID: "d1", ChannelID: "c1", RequestID: requestID,
		Purpose: whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_REQUEST, Ciphertext: []byte("sealed"),
	}
}

func TestStagerStageRejectsIncompleteScopes(t *testing.T) {
	cases := []struct {
		name    string
		mutate  func(*Stager, *stagerStore, *stagerPayloads, *broker.StageRequest)
		want    error
		minimal bool
	}{
		{"empty ciphertext", func(_ *Stager, _ *stagerStore, _ *stagerPayloads, r *broker.StageRequest) {
			r.Ciphertext = nil
		}, broker.ErrStagingFailed, false},
		{"missing request identity", func(_ *Stager, _ *stagerStore, _ *stagerPayloads, r *broker.StageRequest) {
			r.RequestID = ""
		}, broker.ErrStagingFailed, false},
		{"request lookup failure", func(_ *Stager, s *stagerStore, _ *stagerPayloads, _ *broker.StageRequest) {
			s.err = errors.New("db down")
		}, broker.ErrPeerTransportUnsupported, false},
		{"request host mismatch", func(_ *Stager, s *stagerStore, _ *stagerPayloads, _ *broker.StageRequest) {
			s.request.HostID = "other"
		}, broker.ErrStagingFailed, false},
		{"missing grant stamp", func(_ *Stager, s *stagerStore, _ *stagerPayloads, _ *broker.StageRequest) {
			s.request.Stamp = nil
		}, broker.ErrStagingFailed, false},
		{"channel mismatch", func(_ *Stager, s *stagerStore, _ *stagerPayloads, r *broker.StageRequest) {
			r.ChannelID = "other"
		}, broker.ErrStagingFailed, false},
		{"unsupported purpose", func(_ *Stager, _ *stagerStore, _ *stagerPayloads, r *broker.StageRequest) {
			r.Purpose = whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_ATTACHMENT_PART
		}, broker.ErrStagingFailed, false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			service, state, payloads := stagerFixture(t)
			request := requestStage("r1")
			tc.mutate(service, state, payloads, &request)
			if _, err := service.Stage(context.Background(), request); !errors.Is(err, tc.want) {
				t.Fatalf("err = %v, want %v", err, tc.want)
			}
		})
	}
}

func TestStagerStagesEventUnderCurrentGrant(t *testing.T) {
	service, _, payloads := stagerFixture(t)
	reference, err := service.Stage(context.Background(), broker.StageRequest{
		HostID: "h1", DeviceID: "d1", ChannelID: "c1", MessageID: "event-1",
		Purpose: whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_EVENT, Ciphertext: []byte("event"),
	})
	if err != nil {
		t.Fatal(err)
	}
	if reference.GetPurpose() != whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_EVENT {
		t.Fatalf("reference = %+v", reference)
	}
	if len(payloads.inputs) != 1 || payloads.inputs[0].MessageID != "event-1" || payloads.inputs[0].RequestID != "" {
		t.Fatalf("event scope = %+v", payloads.inputs)
	}
}

func TestStagerStageEventRejectsIncompleteScopeAndInactiveGrant(t *testing.T) {
	service, state, _ := stagerFixture(t)
	base := broker.StageRequest{
		HostID: "h1", DeviceID: "d1", ChannelID: "c1", MessageID: "event-1",
		Purpose: whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_EVENT, Ciphertext: []byte("event"),
	}
	missing := base
	missing.MessageID = ""
	if _, err := service.Stage(context.Background(), missing); !errors.Is(err, broker.ErrStagingFailed) {
		t.Fatalf("incomplete event = %v", err)
	}
	state.grant.Active = false
	if _, err := service.Stage(context.Background(), base); !errors.Is(err, broker.ErrPeerTransportUnsupported) {
		t.Fatalf("inactive grant = %v", err)
	}
}

func TestStagerFinishFailureCancelsAndStaysExplicit(t *testing.T) {
	service, _, payloads := stagerFixture(t)
	payloads.finishErr = errors.New("rename failed")
	if _, err := service.Stage(context.Background(), requestStage("r1")); !errors.Is(err, broker.ErrPeerTransportUnsupported) {
		t.Fatalf("finish failure = %v", err)
	}
	if len(payloads.cancelled) != 1 {
		t.Fatal("failed finish was not cancelled")
	}
}

func TestStagerCheckRejectsInvalidReferencesAndExpiredObjects(t *testing.T) {
	service, state, _ := stagerFixture(t)
	scope := store.PayloadCheckScope{Purpose: whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_RESULT}
	if err := service.CheckPayload(context.Background(), nil, scope); !errors.Is(err, payload.ErrInvalid) {
		t.Fatalf("nil reference = %v", err)
	}
	reference := &whalereinv1.PayloadReference{
		ObjectId: "0123456789abcdef0123456789abcdef", OwnerNodeId: "other",
		CiphertextBytes: 1, Sha256: make([]byte, 32),
		Purpose: whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_RESULT,
	}
	if err := service.CheckPayload(context.Background(), reference, scope); !errors.Is(err, payload.ErrForbidden) {
		t.Fatalf("foreign owner = %v", err)
	}
	reference.OwnerNodeId = "node-1"
	state.object = store.PayloadObject{State: store.PayloadReserved}
	if err := service.CheckPayload(context.Background(), reference, scope); !errors.Is(err, payload.ErrExpired) {
		t.Fatalf("reserved object = %v", err)
	}
	state.object = store.PayloadObject{
		State: store.PayloadReady, ExpiresAt: time.UnixMilli(1_700_000_100_000),
		Length: 2, Digest: make([]byte, 32), Purpose: "PAYLOAD_PURPOSE_RESULT",
	}
	if err := service.CheckPayload(context.Background(), reference, scope); !errors.Is(err, payload.ErrForbidden) {
		t.Fatalf("length mismatch = %v", err)
	}
	state.object.Length = 1
	if err := service.CheckPayload(context.Background(), reference, scope); !errors.Is(err, payload.ErrForbidden) {
		t.Fatalf("missing grant stamp = %v", err)
	}
}

func TestNewStagerValidatesEnabledConfiguration(t *testing.T) {
	if _, err := NewStager(nil, nil, StagerConfig{Enabled: true}); err == nil {
		t.Fatal("missing seams accepted")
	}
	state := &stagerStore{}
	payloads := &stagerPayloads{}
	if _, err := NewStager(state, payloads, StagerConfig{Enabled: true}); err == nil {
		t.Fatal("missing node accepted")
	}
	if _, err := NewStager(state, payloads, StagerConfig{Enabled: true, NodeID: "n"}); err == nil {
		t.Fatal("missing retention accepted")
	}
	if _, err := NewStager(state, payloads, StagerConfig{Enabled: true, NodeID: "n", Retention: time.Hour, OwnerID: "owner"}); err != nil {
		t.Fatal(err)
	}
}
