package broker

// Complete-message relay: one MQTT PUBLISH per application message. A packet
// that fits every hop is published inline; an oversized durable body is staged
// as exact ciphertext and published only as a packet-sized reference. There is
// no fragment, reassembly or application receipt protocol.

import (
	"bytes"
	"context"
	"crypto/sha256"
	"fmt"
	"time"

	"github.com/whalerein/whalerein/access/internal/config"
	"google.golang.org/protobuf/proto"

	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// StageRequest carries the exact opaque ciphertext Access is about to
// reference. It never contains plaintext, filenames or credentials. MessageID
// is correlation metadata for the stager's audit trail; it confers no
// authority and is not part of the wire reference.
type StageRequest struct {
	HostID    string
	DeviceID  string
	ChannelID string
	// SessionID and RequestID bind a staged body to the original scope. They
	// are correlation metadata; the stager re-reads the authoritative request
	// row and its grant stamp before reserving.
	SessionID  string
	RequestID  string
	Purpose    whalereinv1.PayloadPurpose
	PartID     string
	MessageID  string
	ExpiresAt  time.Time
	Ciphertext []byte
}

// PayloadStager stages one complete ciphertext body and returns the opaque
// reference. The implementation owns durability, scope, quota and expiry; the
// relay only guarantees that Stage returns before any reference is published.
// A nil PayloadStager makes an oversized hop fail explicitly before publish.
type PayloadStager interface {
	Stage(ctx context.Context, request StageRequest) (*whalereinv1.PayloadReference, error)
}

// mqttVarintSize returns the MQTT variable-byte-integer length of value.
func mqttVarintSize(value int) int {
	if value < 0 {
		value = 0
	}
	switch {
	case value < 1<<7:
		return 1
	case value < 1<<14:
		return 2
	case value < 1<<21:
		return 3
	default:
		return 4
	}
}

// encodedPublishBytes is the exact MQTT 5 QoS 1 PUBLISH size for one topic and
// payload:
//
//	remaining = 2 + len(topic) + 2 + varint(len(properties))
//	          + len(properties) + len(payload)
//	encoded   = 1 + varint(remaining) + remaining
//
// The leading byte is the PUBLISH fixed header with QoS 1, the first two bytes
// are the topic length prefix and the second two are the packet identifier. No
// hop may estimate from ciphertext length alone.
func encodedPublishBytes(topic string, payloadBytes, propertyBytes int) int {
	if payloadBytes < 0 {
		payloadBytes = 0
	}
	if propertyBytes < 0 {
		propertyBytes = 0
	}
	remaining := 2 + len(topic) + 2 + mqttVarintSize(propertyBytes) + propertyBytes + payloadBytes
	return 1 + mqttVarintSize(remaining) + remaining
}

// encodedLivePublishBytes is the exact MQTT 5 QoS 0 PUBLISH size. A QoS 0
// packet carries no packet identifier, so it is two bytes smaller than the
// QoS 1 formula above.
func encodedLivePublishBytes(topic string, payloadBytes, propertyBytes int) int {
	if payloadBytes < 0 {
		payloadBytes = 0
	}
	if propertyBytes < 0 {
		propertyBytes = 0
	}
	remaining := 2 + len(topic) + mqttVarintSize(propertyBytes) + propertyBytes + payloadBytes
	return 1 + mqttVarintSize(remaining) + remaining
}

// selectPacketLimit returns the smallest positive complete-packet limit among
// the local send profile, the broker limit and (optionally) the advertised
// receiver budget. A zero value means "not advertised" and is ignored.
func selectPacketLimit(local uint32, broker uint64, advertised *config.TransportEffective) uint64 {
	limit := uint64(local)
	if broker > 0 && (limit == 0 || broker < limit) {
		limit = broker
	}
	if advertised != nil {
		if value := uint64(advertised.PacketBytes); value > 0 && (limit == 0 || value < limit) {
			limit = value
		}
	}
	if limit == 0 {
		limit = whalereinv1.BrokerPacketLimitBytes
	}
	return limit
}

// accessReceiveAdvertisement renders Access's own receive limits for the
// Host -> Access hop. generation is scoped to the Host broker connection that
// carries the advertisement.
func accessReceiveAdvertisement(local config.TransportEffective, generation uint64) *whalereinv1.ReceiveAdvertisement {
	transfer := local.TransferTimeout
	if transfer <= 0 {
		transfer = time.Minute
	}
	return &whalereinv1.ReceiveAdvertisement{
		Profile: local.WireProfile,
		Budget: &whalereinv1.TransportBudget{
			PacketBytes:       local.PacketBytes,
			EnvelopeBytes:     local.EnvelopeBytes,
			BulkConcurrency:   local.BulkConcurrency,
			ReservedBytes:     local.ReservedBytes,
			TransferTimeoutMs: uint32(transfer.Milliseconds()),
		},
		ConnectionGeneration: generation,
		RelayVersion:         RelayEpoch,
	}
}

// inlineCiphertext extracts the exact opaque body and its reference purpose
// from one outbound envelope. A message that already carries a reference needs
// no staging. A bootstrap envelope that must stay inline returns ok=false.
func inlineCiphertext(topic string, payload []byte) (body []byte, request StageRequest, ok bool, alreadyReferenced bool, err error) {
	parsed, parseErr := ParseTopic(topic)
	if parseErr != nil {
		return nil, StageRequest{}, false, false, transportReject(transportCodeTopic, ErrInvalidTopic)
	}
	switch parsed.Kind {
	case KindRequest:
		var message whalereinv1.HostRequest
		if err := proto.Unmarshal(payload, &message); err != nil {
			return nil, StageRequest{}, false, false, transportReject(transportCodeHeader, ErrPeerTransportUnsupported)
		}
		if message.GetBodyReference() != nil {
			return nil, StageRequest{}, false, true, nil
		}
		if len(message.GetBody()) == 0 {
			return nil, StageRequest{}, false, false, nil
		}
		request = StageRequest{
			HostID:     firstNonEmpty(message.GetHostId(), parsed.HostID),
			DeviceID:   message.GetDeviceId(),
			ChannelID:  message.GetChannelId(),
			RequestID:  message.GetRequestId(),
			Purpose:    whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_REQUEST,
			MessageID:  message.GetRequestId() + "\x00" + message.GetCommandId(),
			Ciphertext: message.GetBody(),
		}
		if expires := message.GetExpiresAtUnixMs(); expires > 0 {
			request.ExpiresAt = time.UnixMilli(expires).UTC()
		}
		return message.GetBody(), request, true, false, nil
	case KindResult:
		var message whalereinv1.HostResult
		if err := proto.Unmarshal(payload, &message); err != nil {
			return nil, StageRequest{}, false, false, transportReject(transportCodeHeader, ErrPeerTransportUnsupported)
		}
		if message.GetResultReference() != nil {
			return nil, StageRequest{}, false, true, nil
		}
		if len(message.GetResultBody()) == 0 {
			return nil, StageRequest{}, false, false, nil
		}
		request = StageRequest{
			HostID:     firstNonEmpty(message.GetHostId(), parsed.HostID),
			DeviceID:   message.GetDeviceId(),
			ChannelID:  message.GetChannelId(),
			RequestID:  message.GetRequestId(),
			Purpose:    whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_RESULT,
			MessageID:  message.GetRequestId() + "\x00" + message.GetCommandId(),
			Ciphertext: message.GetResultBody(),
		}
		return message.GetResultBody(), request, true, false, nil
	case KindDeviceEvent:
		var message whalereinv1.EventEnvelope
		if err := proto.Unmarshal(payload, &message); err != nil {
			return nil, StageRequest{}, false, false, transportReject(transportCodeHeader, ErrPeerTransportUnsupported)
		}
		if message.GetPayloadReference() != nil {
			return nil, StageRequest{}, false, true, nil
		}
		if len(message.GetCiphertext()) == 0 {
			return nil, StageRequest{}, false, false, nil
		}
		request = StageRequest{
			HostID:     firstNonEmpty(message.GetHostId(), parsed.HostID),
			DeviceID:   firstNonEmpty(message.GetDeviceId(), parsed.DeviceID),
			ChannelID:  message.GetChannelId(),
			Purpose:    whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_EVENT,
			MessageID:  fmt.Sprintf("%s\x00%s\x00%d", message.GetChannelId(), message.GetDeviceId(), message.GetCreatedAtUnixMs()),
			Ciphertext: message.GetCiphertext(),
		}
		return message.GetCiphertext(), request, true, false, nil
	default:
		// Notices are packet-sized metadata only; presence and control payloads
		// are never relayed as content.
		return nil, StageRequest{}, false, false, nil
	}
}

func firstNonEmpty(values ...string) string {
	for _, value := range values {
		if value != "" {
			return value
		}
	}
	return ""
}

// verifyReference proves the stager returned a reference for the exact staged
// ciphertext. A substituted, truncated or mismatched object is never published.
func verifyReference(reference *whalereinv1.PayloadReference, body []byte, purpose whalereinv1.PayloadPurpose) error {
	if reference == nil || reference.GetObjectId() == "" || reference.GetOwnerNodeId() == "" {
		return fmt.Errorf("%w: incomplete reference", ErrStagingFailed)
	}
	if reference.GetPurpose() != purpose {
		return fmt.Errorf("%w: reference purpose mismatch", ErrStagingFailed)
	}
	if reference.GetCiphertextBytes() != uint64(len(body)) {
		return fmt.Errorf("%w: reference length mismatch", ErrStagingFailed)
	}
	digest := sha256.Sum256(body)
	if len(reference.GetSha256()) != 0 && !bytes.Equal(reference.GetSha256(), digest[:]) {
		return fmt.Errorf("%w: reference digest mismatch", ErrStagingFailed)
	}
	if reference.GetExpiresAtUnixMs() <= 0 {
		return fmt.Errorf("%w: reference expiry missing", ErrStagingFailed)
	}
	return nil
}

// rewriteToReference stages the exact inline ciphertext and returns the same
// envelope with the matching reference field set and the inline body cleared.
// Nothing is returned for publish until staging completed.
func rewriteToReference(ctx context.Context, topic string, payload []byte, stager PayloadStager) ([]byte, bool, error) {
	body, request, ok, alreadyReferenced, err := inlineCiphertext(topic, payload)
	if err != nil {
		return nil, false, err
	}
	if alreadyReferenced {
		return payload, true, nil
	}
	if !ok {
		// A small bootstrap envelope (or a metadata-only notice) has no durable
		// body to stage and therefore no reference representation.
		return nil, false, ErrReferenceTooLarge
	}
	if stager == nil {
		return nil, false, ErrStagingUnavailable
	}
	reference, err := stager.Stage(ctx, request)
	if err != nil {
		return nil, false, fmt.Errorf("%w: %w", ErrStagingFailed, err)
	}
	if err := verifyReference(reference, body, request.Purpose); err != nil {
		return nil, false, err
	}
	parsed, err := ParseTopic(topic)
	if err != nil {
		return nil, false, transportReject(transportCodeTopic, ErrInvalidTopic)
	}
	switch parsed.Kind {
	case KindRequest:
		var message whalereinv1.HostRequest
		if err := proto.Unmarshal(payload, &message); err != nil {
			return nil, false, transportReject(transportCodeHeader, ErrPeerTransportUnsupported)
		}
		message.Body = nil
		message.BodyReference = reference
		encoded, err := proto.Marshal(&message)
		if err != nil {
			return nil, false, fmt.Errorf("marshal host request: %w", err)
		}
		return encoded, true, nil
	case KindResult:
		var message whalereinv1.HostResult
		if err := proto.Unmarshal(payload, &message); err != nil {
			return nil, false, transportReject(transportCodeHeader, ErrPeerTransportUnsupported)
		}
		message.ResultBody = nil
		message.ResultReference = reference
		encoded, err := proto.Marshal(&message)
		if err != nil {
			return nil, false, fmt.Errorf("marshal host result: %w", err)
		}
		return encoded, true, nil
	case KindDeviceEvent:
		var message whalereinv1.EventEnvelope
		if err := proto.Unmarshal(payload, &message); err != nil {
			return nil, false, transportReject(transportCodeHeader, ErrPeerTransportUnsupported)
		}
		message.Ciphertext = nil
		message.PayloadReference = reference
		encoded, err := proto.Marshal(&message)
		if err != nil {
			return nil, false, fmt.Errorf("marshal event envelope: %w", err)
		}
		return encoded, true, nil
	default:
		return nil, false, ErrReferenceTooLarge
	}
}
