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

// Admission is the local grant authority used for each event. Check never
// blocks: an unknown grant drops the live event and schedules a bounded load,
// so a cold lookup cannot stall unrelated forwarding.
type Admission interface {
	Check(hostID, deviceID string) (allowed bool, known bool)
}

// Broker publishes control events and lossy live previews to the device topic.
type Broker interface {
	PublishDeviceEvent(ctx context.Context, hostID, deviceID string, payload []byte) error
	PublishDeviceLive(ctx context.Context, hostID, deviceID string, payload []byte) error
}

// Dependencies wires the event router.
type Dependencies struct {
	Admission Admission
	Broker    Broker
	Metrics   *diagnostics.Metrics
	Masker    *diagnostics.Masker
	Logger    *slog.Logger
}

// Handler implements event routing.
type Handler struct {
	admission Admission
	broker    Broker
	metrics   *diagnostics.Metrics
	masker    *diagnostics.Masker
	logger    *slog.Logger
}

// New constructs an event handler.
func New(deps Dependencies) (*Handler, error) {
	if deps.Admission == nil {
		return nil, errors.New("events: admission is required")
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
		admission: deps.Admission,
		broker:    deps.Broker,
		metrics:   deps.Metrics,
		masker:    deps.Masker,
		logger:    deps.Logger,
	}, nil
}

// Handle validates the envelope against the topic host, checks the live grant
// and republishes the original bytes to the control device topic.
func (h *Handler) Handle(ctx context.Context, topicHost string, payload []byte) error {
	hostID, deviceID, err := h.route(topicHost, payload, "event")
	if err != nil || hostID == "" {
		return err
	}
	if err := h.broker.PublishDeviceEvent(ctx, hostID, deviceID, payload); err != nil {
		return fmt.Errorf("events: publish: %w", err)
	}
	return nil
}

// HandleLive uses the same envelope validation and admission check as control
// events, then republishes the exact QoS 0 preview. It never persists, queues or
// retries: a broker-level drop is a normal nil outcome.
func (h *Handler) HandleLive(ctx context.Context, topicHost string, payload []byte) error {
	hostID, deviceID, err := h.route(topicHost, payload, "live preview")
	if err != nil || hostID == "" {
		return err
	}
	if err := h.broker.PublishDeviceLive(ctx, hostID, deviceID, payload); err != nil {
		return fmt.Errorf("events: live publish: %w", err)
	}
	return nil
}

// route validates one EventEnvelope against its topic and checks the exact
// host/device grant. A denied grant returns an empty host id and nil error:
// the caller drops the frame without surfacing a route failure.
func (h *Handler) route(topicHost string, payload []byte, kind string) (string, string, error) {
	if topicHost == "" {
		return "", "", errors.New("events: topic host is required")
	}
	var envelope whalereinv1.EventEnvelope
	if err := proto.Unmarshal(payload, &envelope); err != nil {
		return "", "", fmt.Errorf("events: decode envelope: %w", err)
	}
	if envelope.GetHostId() == "" {
		envelope.HostId = topicHost
	} else if envelope.GetHostId() != topicHost {
		return "", "", errors.New("events: host mismatch")
	}
	deviceID := envelope.GetDeviceId()
	if deviceID == "" {
		return "", "", errors.New("events: device id is required")
	}
	active, known := h.admission.Check(topicHost, deviceID)
	if !active {
		h.metrics.EventDropped()
		if !known {
			h.logger.Debug(kind+" dropped before the grant was known",
				"host", h.masker.Host(topicHost), "device", h.masker.Device(deviceID))
		} else {
			h.logger.Debug(kind+" dropped",
				"host", h.masker.Host(topicHost), "device", h.masker.Device(deviceID))
		}
		return "", "", nil
	}
	return topicHost, deviceID, nil
}
