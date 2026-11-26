// Package observation implements Access's producer role in the WhaleRein
// latency-observation contract.
//
// The package emits typed, content-free facts at the boundaries Access owns:
// query admission/forward/reply/expiry, durable command admission/receipt/
// recovery/terminal, relay object staging/transfer/verification, broker
// connection/subscription notices and broker forwarding facts. The normative
// event schema is contracts/latency/observation.json; this package does not
// define a second vocabulary.
//
// One bounded outlet exists per process and is shared by every Access module.
// It is created only in trace mode. In off and aggregate mode Open returns a
// nil sink and every emit is a nil-receiver return, so no writer, channel,
// tokenizer or clock exists. A full, stalled or failing outlet drops records,
// counts loss and never blocks business handling.
package observation

import (
	"bufio"
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"os"
	"strconv"
)

// Manifest is the checked-in, language-neutral event contract. Every producer
// in this repository validates against the same file so a field, enum or
// producer-role drift fails a contract test instead of silently changing the
// evidence.
type Manifest struct {
	Schema          int                  `json:"schema"`
	Name            string               `json:"name"`
	CaptureSchema   int                  `json:"capture_schema"`
	WorkloadVersion int                  `json:"workload_version"`
	Clock           ClockSpec            `json:"clock"`
	Producers       []string             `json:"producers"`
	Header          HeaderSpec           `json:"header"`
	Events          map[string]EventSpec `json:"events"`
	CoveragePaths   map[string][]string  `json:"coverage_paths"`
	RejoinFields    []string             `json:"rejoin_fields"`
	ForbiddenFields []string             `json:"forbidden_fields"`
}

// ClockSpec is the single measurement clock every producer must read.
type ClockSpec struct {
	Source   string `json:"source"`
	Unit     string `json:"unit"`
	Epoch    string `json:"epoch"`
	Topology string `json:"topology"`
}

// HeaderSpec lists the common record fields and the schema version they carry.
type HeaderSpec struct {
	Required []string `json:"required"`
	V        int      `json:"v"`
}

// EventSpec declares the producer roles, required/optional fields and field
// types for one event name.
type EventSpec struct {
	Producers []string          `json:"producers"`
	Required  []string          `json:"required"`
	Optional  []string          `json:"optional"`
	Fields    map[string]string `json:"fields"`
}

// LoadManifest reads and minimally validates the manifest file.
func LoadManifest(path string) (*Manifest, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	var manifest Manifest
	if err := json.Unmarshal(data, &manifest); err != nil {
		return nil, fmt.Errorf("observation: decode manifest: %w", err)
	}
	if manifest.Schema == 0 || manifest.Header.V == 0 || len(manifest.Events) == 0 {
		return nil, errors.New("observation: manifest is incomplete")
	}
	if manifest.Clock.Source != "CLOCK_MONOTONIC" || manifest.Clock.Unit != "ns" {
		return nil, fmt.Errorf("observation: unsupported clock %s/%s", manifest.Clock.Source, manifest.Clock.Unit)
	}
	return &manifest, nil
}

// DecodeRecord decodes one NDJSON record while preserving integer precision.
func DecodeRecord(line []byte) (map[string]any, error) {
	decoder := json.NewDecoder(bytes.NewReader(line))
	decoder.UseNumber()
	var record map[string]any
	if err := decoder.Decode(&record); err != nil {
		return nil, err
	}
	if decoder.More() {
		return nil, errors.New("observation: trailing data after record")
	}
	return record, nil
}

// Validate checks one decoded record against the manifest: header shape,
// producer role, known event, required/optional fields, field types, enum
// membership, non-negative integer timestamps and the forbidden-field
// allowlist. It is used by contract tests and by callers that want to verify
// a captured line before handing it to the fixture.
func (m *Manifest) Validate(record map[string]any) error {
	if m == nil {
		return errors.New("observation: nil manifest")
	}
	for key := range record {
		if !contains(m.Header.Required, key) {
			return fmt.Errorf("observation: unknown header field %q", key)
		}
	}
	for _, key := range m.Header.Required {
		if _, ok := record[key]; !ok {
			return fmt.Errorf("observation: missing header field %q", key)
		}
	}
	if err := rejectForbidden(m.ForbiddenFields, record); err != nil {
		return err
	}
	version, ok := unsigned(record["v"])
	if !ok || version != uint64(m.Header.V) {
		return fmt.Errorf("observation: unsupported record version %v", record["v"])
	}
	run, ok := record["run"].(string)
	if !ok || run == "" {
		return errors.New("observation: run must be a non-empty string")
	}
	producer, ok := record["producer"].(string)
	if !ok || !contains(m.Producers, producer) {
		return fmt.Errorf("observation: unknown producer %v", record["producer"])
	}
	lifetime, ok := record["lifetime"].(string)
	if !ok || lifetime == "" {
		return errors.New("observation: lifetime must be a non-empty string")
	}
	if _, ok := unsigned(record["seq"]); !ok {
		return errors.New("observation: seq must be an unsigned integer")
	}
	if _, ok := unsigned(record["ts"]); !ok {
		return errors.New("observation: ts must be an unsigned integer")
	}
	event, ok := record["event"].(string)
	if !ok {
		return errors.New("observation: event must be a string")
	}
	spec, ok := m.Events[event]
	if !ok {
		return fmt.Errorf("observation: unknown event %q", event)
	}
	if !contains(spec.Producers, producer) {
		return fmt.Errorf("observation: producer %q may not emit %q", producer, event)
	}
	data, ok := record["data"].(map[string]any)
	if !ok {
		return errors.New("observation: data must be an object")
	}
	for _, key := range spec.Required {
		if _, ok := data[key]; !ok {
			return fmt.Errorf("observation: %s is missing required field %q", event, key)
		}
	}
	for key, value := range data {
		if contains(m.ForbiddenFields, key) {
			return fmt.Errorf("observation: forbidden field %q", key)
		}
		fieldType, known := spec.Fields[key]
		if !known {
			return fmt.Errorf("observation: %s has unknown field %q", event, key)
		}
		if !contains(spec.Required, key) && !contains(spec.Optional, key) {
			return fmt.Errorf("observation: %s field %q is not declared", event, key)
		}
		if err := validateField(m, fieldType, value); err != nil {
			return fmt.Errorf("observation: %s.%s: %w", event, key, err)
		}
	}
	return nil
}

func rejectForbidden(forbidden []string, value any) error {
	switch typed := value.(type) {
	case map[string]any:
		for key, nested := range typed {
			if contains(forbidden, key) {
				return fmt.Errorf("observation: forbidden field %q", key)
			}
			if err := rejectForbidden(forbidden, nested); err != nil {
				return err
			}
		}
	case []any:
		for _, nested := range typed {
			if err := rejectForbidden(forbidden, nested); err != nil {
				return err
			}
		}
	}
	return nil
}

func validateField(m *Manifest, fieldType string, value any) error {
	switch {
	case fieldType == "u64":
		if _, ok := unsigned(value); !ok {
			return fmt.Errorf("expected unsigned integer, got %T", value)
		}
	case fieldType == "bool":
		if _, ok := value.(bool); !ok {
			return fmt.Errorf("expected bool, got %T", value)
		}
	case fieldType == "str":
		if _, ok := value.(string); !ok {
			return fmt.Errorf("expected string, got %T", value)
		}
	case fieldType == "producer":
		name, ok := value.(string)
		if !ok || !contains(m.Producers, name) {
			return fmt.Errorf("expected producer role, got %v", value)
		}
	case len(fieldType) > 5 && fieldType[:5] == "enum:":
		name, ok := value.(string)
		if !ok {
			return fmt.Errorf("expected enum string, got %T", value)
		}
		allowed := splitEnum(fieldType[5:])
		if !contains(allowed, name) {
			return fmt.Errorf("value %q is not in %v", name, allowed)
		}
	default:
		return fmt.Errorf("unsupported manifest type %q", fieldType)
	}
	return nil
}

func unsigned(value any) (uint64, bool) {
	switch typed := value.(type) {
	case json.Number:
		parsed, err := strconv.ParseUint(typed.String(), 10, 64)
		if err != nil {
			return 0, false
		}
		return parsed, true
	case float64:
		if typed < 0 || typed != math.Trunc(typed) || typed > math.MaxUint64 {
			return 0, false
		}
		return uint64(typed), true
	case int:
		if typed < 0 {
			return 0, false
		}
		return uint64(typed), true
	case int64:
		if typed < 0 {
			return 0, false
		}
		return uint64(typed), true
	case uint64:
		return typed, true
	case uint:
		return uint64(typed), true
	default:
		return 0, false
	}
}

func splitEnum(value string) []string {
	parts := make([]string, 0, 4)
	start := 0
	for index := 0; index <= len(value); index++ {
		if index == len(value) || value[index] == ',' {
			parts = append(parts, value[start:index])
			start = index + 1
		}
	}
	return parts
}

func contains(values []string, want string) bool {
	for _, value := range values {
		if value == want {
			return true
		}
	}
	return false
}

// ReadRecords reads every NDJSON record from one captured outlet file. It is
// the reader used by fixture and contract tests; malformed or partial lines
// are an error rather than silently ignored.
func ReadRecords(path string) ([]map[string]any, error) {
	file, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer file.Close()
	records := make([]map[string]any, 0, 64)
	scanner := bufio.NewScanner(file)
	scanner.Buffer(make([]byte, 0, 64*1024), 4<<20)
	for scanner.Scan() {
		record, err := DecodeRecord(scanner.Bytes())
		if err != nil {
			return nil, err
		}
		records = append(records, record)
	}
	if err := scanner.Err(); err != nil {
		return nil, err
	}
	return records, nil
}
