package config

import (
	"errors"
	"fmt"
	"math"
	"strings"
	"time"

	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// TransportConfig selects one packaged transport profile and optionally applies
// explicit overrides. The packaged profile vocabulary lives in
// proto/v1/transport_profiles.json and is generated into the shared Go
// bindings. All byte counts are encoded bytes.
//
// Pointer fields distinguish an omitted override from an explicit zero. A zero
// or negative value is never a valid byte or count; it fails validation.
type TransportConfig struct {
	Profile string `yaml:"profile"`

	PacketBytes         *uint32   `yaml:"packet_bytes"`
	PacketOverheadBytes *uint32   `yaml:"packet_overhead_bytes"`
	EnvelopeBytes       *uint64   `yaml:"envelope_bytes"`
	BulkConcurrency     *uint32   `yaml:"bulk_concurrency"`
	ReservedBytes       *uint64   `yaml:"reserved_bytes"`
	TransferTimeout     *Duration `yaml:"transfer_timeout"`

	// Ordered publisher flow control. ReceiveMaximum is advertised in CONNECT;
	// the queue depth and byte bound cap waiting work per destination flow and
	// across the publisher. SendInFlight bounds concurrent QoS 1 publications.
	ReceiveMaximum *uint16 `yaml:"receive_maximum"`
	SendQueueDepth *uint32 `yaml:"send_queue_depth"`
	SendQueueBytes *uint64 `yaml:"send_queue_bytes"`
	SendInFlight   *uint32 `yaml:"send_in_flight"`

	// ResultLimitBytes is Access-only: the raw ciphertext retained for one
	// encrypted result. It is not a wire field. When omitted it derives from
	// the selected profile envelope.
	ResultLimitBytes *int64 `yaml:"result_limit_bytes"`
}

// Default ordered-publisher bounds. They are deliberately independent of the
// packaged transport profile so the wire vocabulary stays limited to encoded
// receive limits. A MQTT 5 peer that advertises a smaller Receive Maximum
// lowers the effective in-flight bound; it never raises the local one.
const (
	defaultReceiveMaximum uint16 = 16
	defaultSendQueueDepth uint32 = 1024
	defaultSendQueueBytes uint64 = 32 << 20
	defaultSendInFlight   uint32 = 16
	maxReceiveMaximum     uint16 = 65535
)

// TransportEffective is one fully resolved and validated transport budget.
// HTTPBodyBytes and ResultLimitBytes are local Access bounds derived from the
// profile; every other value is the encoded transport budget or a bounded
// local publisher setting.
type TransportEffective struct {
	Profile             string
	WireName            string
	WireProfile         whalereinv1.TransportProfile
	PacketBytes         uint32
	PacketOverheadBytes uint32
	EnvelopeBytes       uint64
	BulkConcurrency     uint32
	ReservedBytes       uint64
	TransferTimeout     time.Duration
	ResultLimitBytes    int64
	HTTPBodyBytes       int64

	// Ordered publisher settings. Zero means "use the packaged default"; the
	// accessor methods below normalize an unset value so callers never derive
	// an unbounded queue from a zero field.
	ReceiveMaximum uint16
	SendQueueDepth uint32
	SendQueueBytes uint64
	SendInFlight   uint32
}

// resultBodyOverheadBytes is the JSON/protobuf framing allowance added to the
// base64-expanded ciphertext when deriving the Access HTTP result-body budget.
// It is deliberately a fixed, documented allowance rather than a compiled byte
// ceiling: the ceiling itself comes from the selected profile.
const resultBodyOverheadBytes = 2 << 20

// DefaultTransportEffective returns the packaged default profile as an
// effective budget without an HTTP result-body override.
func DefaultTransportEffective() TransportEffective {
	values, ok := whalereinv1.TransportProfileValuesFor(whalereinv1.DefaultTransportProfile)
	if !ok {
		panic("config: packaged default transport profile is missing")
	}
	return transportEffectiveFromValues(values)
}

func transportEffectiveFromValues(values whalereinv1.TransportProfileValues) TransportEffective {
	return TransportEffective{
		Profile:             values.Name,
		WireName:            values.WireName,
		WireProfile:         wireProfileFor(values.WireName),
		PacketBytes:         values.PacketBytes,
		PacketOverheadBytes: values.PacketOverheadBytes,
		EnvelopeBytes:       values.EnvelopeBytes,
		BulkConcurrency:     values.BulkConcurrency,
		ReservedBytes:       values.ReservedBytes,
		TransferTimeout:     time.Duration(values.TransferTimeoutMs) * time.Millisecond,
		ReceiveMaximum:      defaultReceiveMaximum,
		SendQueueDepth:      defaultSendQueueDepth,
		SendQueueBytes:      defaultSendQueueBytes,
		SendInFlight:        defaultSendInFlight,
	}
}

func wireProfileFor(wireName string) whalereinv1.TransportProfile {
	for name, value := range whalereinv1.TransportProfile_value {
		if name == wireName {
			return whalereinv1.TransportProfile(value)
		}
	}
	return whalereinv1.TransportProfile_TRANSPORT_PROFILE_UNSPECIFIED
}

// WireNameForProfile maps a packaged profile name to its enum wire name.
func WireNameForProfile(name string) (string, bool) {
	values, ok := whalereinv1.TransportProfileValuesFor(name)
	if !ok {
		return "", false
	}
	return values.WireName, true
}

// ProfileForWireName resolves an enum wire name to the packaged profile name.
func ProfileForWireName(wireName string) (string, bool) {
	for _, name := range whalereinv1.TransportProfileNames() {
		values, ok := whalereinv1.TransportProfileValuesFor(name)
		if ok && values.WireName == wireName {
			return name, true
		}
	}
	return "", false
}

// MaxReceive returns the effective CONNECT Receive Maximum.
func (e TransportEffective) MaxReceive() uint16 {
	if e.ReceiveMaximum == 0 {
		return defaultReceiveMaximum
	}
	return e.ReceiveMaximum
}

// MaxInFlight returns the bounded concurrent QoS 1 publication count.
func (e TransportEffective) MaxInFlight() int {
	value := e.SendInFlight
	if value == 0 {
		value = defaultSendInFlight
	}
	if value > 1024 {
		value = 1024
	}
	return int(value)
}

// QueueDepth returns the per-destination ordered queue bound.
func (e TransportEffective) QueueDepth() int {
	value := e.SendQueueDepth
	if value == 0 {
		value = defaultSendQueueDepth
	}
	if value > 65536 {
		value = 65536
	}
	return int(value)
}

// QueueBytes returns the bounded waiting bytes across one publisher.
func (e TransportEffective) QueueBytes() int64 {
	value := e.SendQueueBytes
	if value == 0 {
		value = defaultSendQueueBytes
	}
	if value > math.MaxInt64 {
		value = math.MaxInt64
	}
	if value < uint64(e.PacketBytes) {
		value = uint64(e.PacketBytes)
	}
	return int64(value)
}

// ceilDiv returns ceil(value/divisor). Callers must reject a zero divisor.
func ceilDiv(value, divisor uint64) uint64 {
	return value/divisor + boolToUint64(value%divisor != 0)
}

func boolToUint64(value bool) uint64 {
	if value {
		return 1
	}
	return 0
}

// Base64EncodedBytes returns the base64 representation size of raw bytes.
func Base64EncodedBytes(raw uint64) uint64 {
	if raw == 0 {
		return 0
	}
	if raw > (math.MaxUint64-2)/4*3 {
		return math.MaxUint64
	}
	return ceilDiv(raw, 3) * 4
}

// DerivedHTTPBodyBytes returns the minimum HTTP response body budget for one
// fully encoded logical envelope: base64 expansion plus JSON framing rounded up
// to a whole MiB.
func DerivedHTTPBodyBytes(envelope uint64) (int64, error) {
	encoded := Base64EncodedBytes(envelope)
	if encoded > math.MaxUint64-resultBodyOverheadBytes {
		return 0, errors.New("http body bound overflows")
	}
	required := encoded + resultBodyOverheadBytes
	const mib = uint64(1 << 20)
	rounded := ceilDiv(required, mib) * mib
	if rounded > math.MaxInt64 {
		return 0, errors.New("http body bound is not representable")
	}
	return int64(rounded), nil
}

func durationMillis(value time.Duration) (uint32, error) {
	if value <= 0 {
		return 0, fmt.Errorf("must be positive")
	}
	millis := value.Milliseconds()
	if millis <= 0 || time.Duration(millis)*time.Millisecond != value {
		return 0, fmt.Errorf("must be a whole number of milliseconds")
	}
	if millis > math.MaxUint32 {
		return 0, fmt.Errorf("is not representable in milliseconds")
	}
	return uint32(millis), nil
}

// ResolveTransport validates the packaged vocabulary and resolves the selected
// profile plus overrides into one effective budget. It also derives and checks
// the Access HTTP result/body bounds. It fails explicitly; it never clamps.
func (c Config) ResolveTransport() (TransportEffective, error) {
	if err := whalereinv1.ValidateTransportProfiles(); err != nil {
		return TransportEffective{}, fmt.Errorf("packaged transport profiles are invalid: %w", err)
	}
	profileName := strings.TrimSpace(c.Transport.Profile)
	if profileName == "" {
		profileName = whalereinv1.DefaultTransportProfile
	}
	values, ok := whalereinv1.TransportProfileValuesFor(profileName)
	if !ok {
		return TransportEffective{}, fmt.Errorf("transport.profile %q is not a packaged profile (have %s)",
			profileName, strings.Join(whalereinv1.TransportProfileNames(), ", "))
	}
	if err := whalereinv1.ValidateTransportProfile(values); err != nil {
		return TransportEffective{}, err
	}
	effective := transportEffectiveFromValues(values)
	if c.Transport.PacketBytes != nil {
		effective.PacketBytes = *c.Transport.PacketBytes
	}
	if c.Transport.PacketOverheadBytes != nil {
		effective.PacketOverheadBytes = *c.Transport.PacketOverheadBytes
	}
	if c.Transport.EnvelopeBytes != nil {
		effective.EnvelopeBytes = *c.Transport.EnvelopeBytes
	}
	if c.Transport.BulkConcurrency != nil {
		effective.BulkConcurrency = *c.Transport.BulkConcurrency
	}
	if c.Transport.ReservedBytes != nil {
		effective.ReservedBytes = *c.Transport.ReservedBytes
	}
	if c.Transport.TransferTimeout != nil {
		effective.TransferTimeout = c.Transport.TransferTimeout.D()
	}
	if c.Transport.ReceiveMaximum != nil {
		if *c.Transport.ReceiveMaximum == 0 {
			return TransportEffective{}, errors.New("transport.receive_maximum must be positive")
		}
		effective.ReceiveMaximum = *c.Transport.ReceiveMaximum
	}
	if c.Transport.SendQueueDepth != nil {
		if *c.Transport.SendQueueDepth == 0 {
			return TransportEffective{}, errors.New("transport.send_queue_depth must be positive")
		}
		effective.SendQueueDepth = *c.Transport.SendQueueDepth
	}
	if c.Transport.SendQueueBytes != nil {
		effective.SendQueueBytes = *c.Transport.SendQueueBytes
	}
	if c.Transport.SendInFlight != nil {
		if *c.Transport.SendInFlight == 0 {
			return TransportEffective{}, errors.New("transport.send_in_flight must be positive")
		}
		effective.SendInFlight = *c.Transport.SendInFlight
	}
	if err := ValidateTransportEffective(effective); err != nil {
		return TransportEffective{}, err
	}
	resultLimit, err := resolveResultLimit(effective.EnvelopeBytes, c.Transport.ResultLimitBytes, c.Requests.MaxResultBytes)
	if err != nil {
		return TransportEffective{}, err
	}
	effective.ResultLimitBytes = resultLimit
	httpBody, err := resolveHTTPBodyBytes(resultLimit, c.HTTP.MaxBodyBytes)
	if err != nil {
		return TransportEffective{}, err
	}
	effective.HTTPBodyBytes = httpBody
	return effective, nil
}

// ValidateTransportEffective checks the profile relations on a resolved budget.
// It is the single relation checker used by startup validation and tests.
// Ordered-publisher fields are optional here: a receive advertisement carries
// only wire limits, so zero local queue values are normalized by the accessors
// instead of rejecting an otherwise valid peer budget.
func ValidateTransportEffective(effective TransportEffective) error {
	var problems []string
	add := func(condition bool, message string) {
		if condition {
			problems = append(problems, message)
		}
	}
	add(effective.PacketBytes == 0, "transport.packet_bytes must be positive")
	add(effective.EnvelopeBytes == 0, "transport.envelope_bytes must be positive")
	add(effective.BulkConcurrency == 0, "transport.bulk_concurrency must be positive")
	add(effective.ReservedBytes == 0, "transport.reserved_bytes must be positive")
	add(uint64(effective.PacketBytes) > whalereinv1.BrokerPacketLimitBytes,
		"transport.packet_bytes must not exceed the broker packet limit")
	add(effective.PacketOverheadBytes >= effective.PacketBytes && effective.PacketBytes > 0,
		"transport.packet_overhead_bytes must be below transport.packet_bytes")
	add(effective.EnvelopeBytes < uint64(effective.PacketBytes),
		"transport.envelope_bytes must cover one complete MQTT packet")
	add(effective.ReservedBytes < effective.EnvelopeBytes,
		"transport.reserved_bytes does not cover one logical envelope")
	if effective.TransferTimeout <= 0 {
		problems = append(problems, "transport.transfer_timeout must be positive")
	}
	if _, err := durationMillis(effective.TransferTimeout); err != nil {
		problems = append(problems, "transport.transfer_timeout "+err.Error())
	}
	if effective.ReceiveMaximum == 0 && effective.SendInFlight == 0 && effective.SendQueueDepth == 0 && effective.SendQueueBytes == 0 {
		// Unresolved peer budget: ordered settings are not part of the wire.
		return joinProblems(problems)
	}
	add(effective.MaxReceive() == 0, "transport.receive_maximum must be positive")
	add(effective.MaxInFlight() <= 0, "transport.send_in_flight must be positive")
	add(effective.QueueDepth() <= 0, "transport.send_queue_depth must be positive")
	add(effective.SendQueueBytes != 0 && effective.SendQueueBytes < uint64(effective.PacketBytes),
		"transport.send_queue_bytes must cover one complete MQTT packet")
	return joinProblems(problems)
}

func joinProblems(problems []string) error {
	if len(problems) > 0 {
		return errors.New("invalid transport configuration: " + strings.Join(problems, "; "))
	}
	return nil
}

func resolveResultLimit(envelope uint64, override *int64, configured int64) (int64, error) {
	if override != nil {
		if *override < 0 {
			return 0, errors.New("transport.result_limit_bytes must be positive")
		}
		if *override > 0 {
			configured = *override
		}
	}
	if configured < 0 {
		return 0, errors.New("requests.max_result_bytes must not be negative")
	}
	if configured > 0 {
		if uint64(configured) > envelope {
			return 0, fmt.Errorf("requests.max_result_bytes %d exceeds the logical envelope %d; the result could never be forwarded",
				configured, envelope)
		}
		return configured, nil
	}
	if envelope > math.MaxInt64 {
		return 0, errors.New("transport.envelope_bytes is not representable as a result bound")
	}
	return int64(envelope), nil
}

func resolveHTTPBodyBytes(resultLimit int64, configured int64) (int64, error) {
	if configured < 0 {
		return 0, errors.New("http.max_body_bytes must not be negative")
	}
	if resultLimit < 0 {
		return 0, errors.New("result bound must not be negative")
	}
	required, err := DerivedHTTPBodyBytes(uint64(resultLimit))
	if err != nil {
		return 0, err
	}
	if configured > 0 {
		if configured < required {
			return 0, fmt.Errorf("http.max_body_bytes %d is below the derived minimum %d for the result bound; a stored result could not be returned",
				configured, required)
		}
		return configured, nil
	}
	return required, nil
}
