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

	PacketBytes          *uint32 `yaml:"packet_bytes"`
	PacketOverheadBytes  *uint32 `yaml:"packet_overhead_bytes"`
	FragmentPayloadBytes *uint32 `yaml:"fragment_payload_bytes"`
	FragmentCount        *uint32 `yaml:"fragment_count"`
	EnvelopeBytes        *uint64 `yaml:"envelope_bytes"`
	BulkConcurrency      *uint32 `yaml:"bulk_concurrency"`
	ReservedBytes        *uint64 `yaml:"reserved_bytes"`

	FragmentIdleTimeout *Duration `yaml:"fragment_idle_timeout"`
	TransferTimeout     *Duration `yaml:"transfer_timeout"`

	// ResultLimitBytes is Access-only: the raw ciphertext retained for one
	// encrypted result. It is not a wire field. When omitted it derives from
	// the selected profile envelope.
	ResultLimitBytes *int64 `yaml:"result_limit_bytes"`
}

// TransportEffective is one fully resolved and validated transport budget.
// HTTPBodyBytes and ResultLimitBytes are local Access bounds derived from the
// profile; every other value is the encoded transport budget.
type TransportEffective struct {
	Profile              string
	WireName             string
	WireProfile          whalereinv1.TransportProfile
	PacketBytes          uint32
	PacketOverheadBytes  uint32
	FragmentPayloadBytes uint32
	FragmentCount        uint32
	EnvelopeBytes        uint64
	BulkConcurrency      uint32
	ReservedBytes        uint64
	FragmentIdleTimeout  time.Duration
	TransferTimeout      time.Duration
	ResultLimitBytes     int64
	HTTPBodyBytes        int64
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
		Profile:              values.Name,
		WireName:             values.WireName,
		WireProfile:          wireProfileFor(values.WireName),
		PacketBytes:          values.PacketBytes,
		PacketOverheadBytes:  values.PacketOverheadBytes,
		FragmentPayloadBytes: values.FragmentPayloadBytes,
		FragmentCount:        values.FragmentCount,
		EnvelopeBytes:        values.EnvelopeBytes,
		BulkConcurrency:      values.BulkConcurrency,
		ReservedBytes:        values.ReservedBytes,
		FragmentIdleTimeout:  time.Duration(values.FragmentIdleTimeoutMs) * time.Millisecond,
		TransferTimeout:      time.Duration(values.TransferTimeoutMs) * time.Millisecond,
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
	if c.Transport.FragmentPayloadBytes != nil {
		effective.FragmentPayloadBytes = *c.Transport.FragmentPayloadBytes
	}
	if c.Transport.FragmentCount != nil {
		effective.FragmentCount = *c.Transport.FragmentCount
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
	if c.Transport.FragmentIdleTimeout != nil {
		effective.FragmentIdleTimeout = c.Transport.FragmentIdleTimeout.D()
	}
	if c.Transport.TransferTimeout != nil {
		effective.TransferTimeout = c.Transport.TransferTimeout.D()
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
func ValidateTransportEffective(effective TransportEffective) error {
	var problems []string
	add := func(condition bool, message string) {
		if condition {
			problems = append(problems, message)
		}
	}
	add(effective.PacketBytes == 0, "transport.packet_bytes must be positive")
	add(effective.FragmentPayloadBytes == 0, "transport.fragment_payload_bytes must be positive")
	add(effective.FragmentCount == 0, "transport.fragment_count must be positive")
	add(effective.EnvelopeBytes == 0, "transport.envelope_bytes must be positive")
	add(effective.BulkConcurrency == 0, "transport.bulk_concurrency must be positive")
	add(effective.ReservedBytes == 0, "transport.reserved_bytes must be positive")
	add(effective.PacketBytes <= effective.FragmentPayloadBytes,
		"transport.packet_bytes must exceed transport.fragment_payload_bytes")
	if effective.PacketBytes > 0 && effective.FragmentPayloadBytes > 0 {
		add(uint64(effective.FragmentPayloadBytes)+uint64(effective.PacketOverheadBytes) > uint64(effective.PacketBytes),
			"transport.fragment_payload_bytes plus packet_overhead_bytes must fit packet_bytes")
	}
	if effective.FragmentPayloadBytes > 0 {
		required := ceilDiv(effective.EnvelopeBytes, uint64(effective.FragmentPayloadBytes))
		add(uint64(effective.FragmentCount) < required,
			"transport.fragment_count does not cover the logical envelope at the selected fragment width")
		add(required > math.MaxUint32, "transport envelope needs more fragments than the wire can express")
	}
	if effective.BulkConcurrency > 0 && effective.FragmentPayloadBytes > 0 {
		inFlight := uint64(effective.BulkConcurrency) * uint64(effective.FragmentPayloadBytes)
		if inFlight/uint64(effective.BulkConcurrency) != uint64(effective.FragmentPayloadBytes) {
			problems = append(problems, "transport in-flight fragment reservation overflows")
		} else {
			add(effective.ReservedBytes < inFlight,
				"transport.reserved_bytes does not cover in-flight fragment payloads")
		}
	}
	add(effective.ReservedBytes < effective.EnvelopeBytes,
		"transport.reserved_bytes does not cover one logical envelope")
	if effective.FragmentIdleTimeout <= 0 {
		problems = append(problems, "transport.fragment_idle_timeout must be positive")
	}
	if effective.TransferTimeout <= 0 {
		problems = append(problems, "transport.transfer_timeout must be positive")
	}
	add(effective.FragmentIdleTimeout > 0 && effective.TransferTimeout > 0 &&
		effective.FragmentIdleTimeout >= effective.TransferTimeout,
		"transport.fragment_idle_timeout must be shorter than transport.transfer_timeout")
	if _, err := durationMillis(effective.FragmentIdleTimeout); err != nil {
		problems = append(problems, "transport.fragment_idle_timeout "+err.Error())
	}
	if _, err := durationMillis(effective.TransferTimeout); err != nil {
		problems = append(problems, "transport.transfer_timeout "+err.Error())
	}
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
