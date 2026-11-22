package relay

// Production adapter between the complete-message broker and the node-local
// temporary ciphertext service. The broker only asks for an opaque reference;
// this file owns scope, grant revisions, exact length/digest, expiry and
// staging through access/internal/payload. It never inspects ciphertext.

import (
	"bytes"
	"context"
	"crypto/sha256"
	"errors"
	"fmt"
	"io"
	"strings"
	"time"

	"github.com/whalerein/whalerein/access/internal/broker"
	"github.com/whalerein/whalerein/access/internal/payload"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// StagerStore is the durable read seam. The payload service owns writes and
// the store owns all SQL.
type StagerStore interface {
	GetRequest(ctx context.Context, requestID string) (store.Request, error)
	AdmissionGrantFor(ctx context.Context, hostID, deviceID string) (store.AdmissionGrant, error)
	GetPayload(ctx context.Context, objectID string) (store.PayloadObject, error)
	StalePayloadScopes(ctx context.Context, limit int) ([]store.PayloadObject, error)
}

// StagerPayloads is the node-local ciphertext seam.
type StagerPayloads interface {
	Reserve(ctx context.Context, input payload.ReserveInput) (payload.Object, error)
	Write(ctx context.Context, ref payload.Ref, body io.Reader) error
	Finish(ctx context.Context, ref payload.Ref) (payload.Object, error)
	Cancel(ctx context.Context, ref payload.Ref) error
	Check(ctx context.Context, ref payload.Ref) (payload.Object, error)
}

// StagerConfig identifies this node and bounds staged lifetimes.
type StagerConfig struct {
	Enabled       bool
	NodeID        string
	OwnerID       string
	Retention     time.Duration
	CleanupBatch  int
	OperationTime time.Duration
}

// Stager implements broker.PayloadStager and the requests PayloadChecker.
type Stager struct {
	store    StagerStore
	payloads StagerPayloads
	config   StagerConfig
	now      func() time.Time
}

// NewStager validates the seam. A disabled stager still returns an object so
// the broker fails explicitly with ErrStagingUnavailable instead of a nil
// pointer.
func NewStager(store StagerStore, payloads StagerPayloads, config StagerConfig) (*Stager, error) {
	if config.Enabled {
		if store == nil || payloads == nil {
			return nil, errors.New("relay stager: store and payloads are required")
		}
		if strings.TrimSpace(config.NodeID) == "" {
			return nil, errors.New("relay stager: a node id is required")
		}
		if config.Retention <= 0 {
			return nil, errors.New("relay stager: a positive retention is required")
		}
	}
	if config.CleanupBatch <= 0 {
		config.CleanupBatch = 100
	}
	if config.OperationTime <= 0 {
		config.OperationTime = time.Minute
	}
	return &Stager{store: store, payloads: payloads, config: config, now: func() time.Time { return time.Now().UTC() }}, nil
}

// Enabled reports whether staging can succeed.
func (s *Stager) Enabled() bool { return s != nil && s.config.Enabled }

func (s *Stager) owner() string {
	if s.config.OwnerID != "" {
		return s.config.OwnerID
	}
	return s.config.NodeID
}

// Stage reserves the exact ciphertext, writes it, publishes it and returns the
// opaque reference only after the object is readable.
func (s *Stager) Stage(ctx context.Context, request broker.StageRequest) (*whalereinv1.PayloadReference, error) {
	if !s.Enabled() || s.payloads == nil || s.store == nil {
		return nil, broker.ErrStagingUnavailable
	}
	if len(request.Ciphertext) == 0 {
		return nil, fmt.Errorf("%w: ciphertext is required", broker.ErrStagingFailed)
	}
	digest := sha256.Sum256(request.Ciphertext)
	input := payload.ReserveInput{
		HostID:         request.HostID,
		DeviceID:       request.DeviceID,
		ChannelID:      request.ChannelID,
		Purpose:        purposeName(request.Purpose),
		PartID:         request.PartID,
		Length:         int64(len(request.Ciphertext)),
		ReservedBytes:  int64(len(request.Ciphertext)),
		Digest:         digest[:],
		ParentDeadline: request.ExpiresAt,
	}
	switch request.Purpose {
	case whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_REQUEST,
		whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_RESULT:
		if request.RequestID == "" {
			return nil, fmt.Errorf("%w: request identity is required", broker.ErrStagingFailed)
		}
		row, err := s.store.GetRequest(ctx, request.RequestID)
		if err != nil {
			return nil, stagingUnavailable(err)
		}
		if row.HostID != request.HostID || (request.DeviceID != "" && row.DeviceID != request.DeviceID) {
			return nil, fmt.Errorf("%w: request scope mismatch", broker.ErrStagingFailed)
		}
		if row.Stamp == nil {
			return nil, fmt.Errorf("%w: request has no grant stamp", broker.ErrStagingFailed)
		}
		input.DeviceID = row.DeviceID
		input.AppSession = row.DeviceSessionID
		input.RequestID = row.RequestID
		if input.ChannelID == "" {
			input.ChannelID = row.ChannelID
		} else if row.ChannelID != "" && input.ChannelID != row.ChannelID {
			return nil, fmt.Errorf("%w: request channel mismatch", broker.ErrStagingFailed)
		}
		input.MessageID = request.MessageID
		if input.MessageID == "" {
			input.MessageID = row.RequestID
		}
		// The parent's fixed deadline always wins over a later wire value.
		input.ParentDeadline = row.ExpiresAt
		input.GrantGeneration = int64Pointer(row.Stamp.GrantGeneration)
		input.RevocationGeneration = int64Pointer(row.Stamp.RevocationGeneration)
		input.DeviceRevocationGeneration = int64Pointer(row.Stamp.DeviceRevocationGeneration)
	case whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_EVENT:
		if request.DeviceID == "" || input.ChannelID == "" || request.MessageID == "" {
			return nil, fmt.Errorf("%w: event scope is incomplete", broker.ErrStagingFailed)
		}
		grant, err := s.store.AdmissionGrantFor(ctx, request.HostID, request.DeviceID)
		if err != nil || !grant.Active {
			return nil, stagingUnavailable(err)
		}
		input.MessageID = request.MessageID
		input.GrantGeneration = int64Pointer(grant.GrantGeneration)
		input.RevocationGeneration = int64Pointer(grant.RevocationGeneration)
		input.DeviceRevocationGeneration = int64Pointer(grant.DeviceRevocationGeneration)
		if input.ParentDeadline.IsZero() {
			input.ParentDeadline = s.now().Add(s.config.Retention)
		}
	default:
		return nil, fmt.Errorf("%w: unsupported staging purpose", broker.ErrStagingFailed)
	}
	object, err := s.payloads.Reserve(ctx, input)
	if err != nil {
		return nil, stagingUnavailable(err)
	}
	ref := payload.Ref{
		ObjectID:   object.ObjectID,
		HostID:     object.HostID,
		DeviceID:   object.DeviceID,
		AppSession: object.AppSession,
		ChannelID:  object.ChannelID,
		Purpose:    object.Purpose,
	}
	if err := s.payloads.Write(ctx, ref, bytes.NewReader(request.Ciphertext)); err != nil {
		_ = s.payloads.Cancel(ctx, ref)
		return nil, stagingUnavailable(err)
	}
	finished, err := s.payloads.Finish(ctx, ref)
	if err != nil {
		_ = s.payloads.Cancel(ctx, ref)
		return nil, stagingUnavailable(err)
	}
	return &whalereinv1.PayloadReference{
		ObjectId:        finished.ObjectID,
		OwnerNodeId:     finished.OwnerID,
		CiphertextBytes: uint64(finished.Length),
		Sha256:          append([]byte(nil), finished.Digest...),
		ExpiresAtUnixMs: finished.ExpiresAt.UnixMilli(),
		Purpose:         request.Purpose,
	}, nil
}

// CheckPayload proves a reference is readable under the exact authorized scope
// without reading ciphertext. Request admission, result status and notices use
// it before advertising a reference.
func (s *Stager) CheckPayload(ctx context.Context, reference *whalereinv1.PayloadReference, scope store.PayloadCheckScope) error {
	if !s.Enabled() || s.store == nil {
		return payload.ErrDisabled
	}
	if reference == nil || objectIDPattern.MatchString(reference.GetObjectId()) == false {
		return payload.ErrInvalid
	}
	if reference.GetOwnerNodeId() == "" || reference.GetOwnerNodeId() != s.owner() {
		return payload.ErrForbidden
	}
	object, err := s.store.GetPayload(ctx, reference.GetObjectId())
	if err != nil {
		return err
	}
	if object.State != store.PayloadReady || !object.ExpiresAt.After(s.now()) {
		return payload.ErrExpired
	}
	if object.Length != int64(reference.GetCiphertextBytes()) ||
		!bytes.Equal(object.Digest, reference.GetSha256()) ||
		object.Purpose != purposeName(scope.Purpose) || reference.GetPurpose() != scope.Purpose {
		return payload.ErrForbidden
	}
	for _, check := range []struct{ expected, actual string }{
		{scope.HostID, object.HostID},
		{scope.DeviceID, object.DeviceID},
		{scope.AppSession, object.AppSession},
		{scope.ChannelID, object.ChannelID},
	} {
		if check.expected != "" && check.expected != check.actual {
			return payload.ErrForbidden
		}
	}
	if scope.RequestID != "" && object.RequestID != "" && scope.RequestID != object.RequestID {
		return payload.ErrForbidden
	}
	if object.GrantGeneration == nil || object.RevocationGeneration == nil || object.DeviceRevocationGeneration == nil {
		return payload.ErrForbidden
	}
	grant, err := s.store.AdmissionGrantFor(ctx, object.HostID, object.DeviceID)
	if err != nil {
		return payload.ErrForbidden
	}
	if !grant.Active ||
		grant.GrantGeneration != uint64(*object.GrantGeneration) ||
		grant.RevocationGeneration != uint64(*object.RevocationGeneration) ||
		grant.DeviceRevocationGeneration != uint64(*object.DeviceRevocationGeneration) {
		return payload.ErrForbidden
	}
	return nil
}

// SweepRevoked cancels a bounded batch of live objects whose original grant or
// managed session no longer authorizes release. Cancellation is best effort: a
// failure stays charged and is retried from the same durable scope.
func (s *Stager) SweepRevoked(ctx context.Context) (int, error) {
	if !s.Enabled() || s.store == nil || s.payloads == nil {
		return 0, nil
	}
	ctx, cancel := context.WithTimeout(ctx, s.config.OperationTime)
	defer cancel()
	objects, err := s.store.StalePayloadScopes(ctx, s.config.CleanupBatch)
	if err != nil {
		return 0, err
	}
	cancelled := 0
	for _, object := range objects {
		if err := s.payloads.Cancel(ctx, payload.Ref{
			ObjectID:   object.ObjectID,
			HostID:     object.HostID,
			DeviceID:   object.DeviceID,
			AppSession: object.AppSession,
			ChannelID:  object.ChannelID,
			Purpose:    object.Purpose,
		}); err != nil {
			continue
		}
		cancelled++
	}
	return cancelled, nil
}

// stagingUnavailable maps payload admission outcomes to the two stable broker
// failures: a size/quota refusal is payload_too_large, everything else is an
// explicit transport unavailability. Nothing is published on either path.
func stagingUnavailable(err error) error {
	if err == nil {
		return broker.ErrPeerTransportUnsupported
	}
	switch {
	case errors.Is(err, payload.ErrTooLarge), errors.Is(err, payload.ErrQuota),
		errors.Is(err, payload.ErrCapacity), errors.Is(err, payload.ErrHeadroom):
		return fmt.Errorf("%w: %w", broker.ErrMessageBudget, err)
	default:
		return fmt.Errorf("%w: %w", broker.ErrPeerTransportUnsupported, err)
	}
}

func int64Pointer(value uint64) *int64 {
	if value > uint64(^uint64(0)>>1) {
		return nil
	}
	converted := int64(value)
	return &converted
}
