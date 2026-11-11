// Package events routes encrypted EventEnvelope messages from a host's event
// topic to exactly one device topic.
//
// Routing uses only the outer device_id. Access never decrypts or inspects the
// ciphertext. Events for revoked or missing grants are dropped.
package events

import (
	"context"
	"errors"
	"fmt"
	"log/slog"

	"github.com/whalerein/whalerein/access/internal/diagnostics"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

// Store is the authorization lookup used for each event.
type Store interface {
	HasActiveGrant(ctx context.Context, hostID, deviceID string) (bool, error)
}

// Broker publishes the event to the device topic.
type Broker interface {
	PublishDeviceEvent(ctx context.Context, hostID, deviceID string, payload []byte) error
}

// Dependencies wires the event router.
type Dependencies struct {
	Store   Store
	Broker  Broker
	Metrics *diagnostics.Metrics
	Masker  *diagnostics.Masker
	Logger  *slog.Logger
}

// Handler implements event routing.
type Handler struct {
	store   Store
	broker  Broker
	metrics *diagnostics.Metrics
	masker  *diagnostics.Masker
	logger  *slog.Logger
}

// New constructs an event handler.
func New(deps Dependencies) (*Handler, error) {
	if deps.Store == nil {
		return nil, errors.New("events: store is required")
	}
	if deps.Broker == nil {
		return nil, errors.New("events: broker is required")
	}
	if deps.Logger == nil {
		deps.Logger = slog.Default()
	}
	if deps.Metrics == nil {
		deps.Metrics = diagnostics.NewMetrics()
	}
	if deps.Masker == nil {
		deps.Masker = diagnostics.NewMasker()
	}
	return &Handler{
		store:   deps.Store,
		broker:  deps.Broker,
		metrics: deps.Metrics,
		masker:  deps.Masker,
		logger:  deps.Logger,
	}, nil
}

// Handle validates the envelope against the topic host, checks the live grant
// and republishes the original bytes to the device topic.
func (h *Handler) Handle(ctx context.Context, topicHost string, payload []byte) error {
	if topicHost == "" {
		return errors.New("events: topic host is required")
	}
	var envelope whalereinv1.EventEnvelope
	if err := proto.Unmarshal(payload, &envelope); err != nil {
		return fmt.Errorf("events: decode envelope: %w", err)
	}
	if envelope.GetHostId() == "" {
		envelope.HostId = topicHost
	} else if envelope.GetHostId() != topicHost {
		return errors.New("events: host mismatch")
	}
	deviceID := envelope.GetDeviceId()
	if deviceID == "" {
		return errors.New("events: device id is required")
	}
	active, err := h.store.HasActiveGrant(ctx, topicHost, deviceID)
	if err != nil {
		return fmt.Errorf("events: check grant: %w", err)
	}
	if !active {
		h.metrics.EventDropped()
		h.logger.Debug("event dropped",
			"host", h.masker.Host(topicHost), "device", h.masker.Device(deviceID))
		return nil
	}
	if err := h.broker.PublishDeviceEvent(ctx, topicHost, deviceID, payload); err != nil {
		return fmt.Errorf("events: publish: %w", err)
	}
	return nil
}
