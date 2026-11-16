package whalereinv1

import (
	"encoding/hex"
	"encoding/json"
	"errors"
	"os"
	"reflect"
	"testing"

	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
)

type compatVector struct {
	ID        string          `json:"id"`
	Kind      string          `json:"kind"`
	Message   string          `json:"message"`
	Languages []string        `json:"languages"`
	JSON      json.RawMessage `json:"json"`
	Hex       string          `json:"hex"`
	Expect    map[string]any  `json:"expect"`
	ErrorCode string          `json:"errorCode"`
}

func loadVectors(t *testing.T) []compatVector {
	t.Helper()
	data, err := os.ReadFile("../../compat/vectors.json")
	if err != nil {
		t.Fatal(err)
	}
	var file struct {
		Schema  string         `json:"schema"`
		Vectors []compatVector `json:"vectors"`
	}
	if err := json.Unmarshal(data, &file); err != nil {
		t.Fatal(err)
	}
	if file.Schema != "whalerein/proto-compat-vectors/v1" {
		t.Fatalf("unexpected vector schema %q", file.Schema)
	}
	return file.Vectors
}

func vectorByID(t *testing.T, id string) compatVector {
	t.Helper()
	for _, vector := range loadVectors(t) {
		if vector.ID == id {
			return vector
		}
	}
	t.Fatalf("missing vector %s", id)
	return compatVector{}
}

func newMessage(t *testing.T, name string) proto.Message {
	t.Helper()
	switch name {
	case "whalerein.v1.HostRequest":
		return &HostRequest{}
	case "whalerein.v1.HostResult":
		return &HostResult{}
	case "whalerein.v1.RequestPayload":
		return &RequestPayload{}
	default:
		t.Fatalf("unsupported vector message %s", name)
		return nil
	}
}

func jsonObject(t *testing.T, message proto.Message) map[string]any {
	t.Helper()
	encoded, err := protojson.Marshal(message)
	if err != nil {
		t.Fatal(err)
	}
	var object map[string]any
	if err := json.Unmarshal(encoded, &object); err != nil {
		t.Fatal(err)
	}
	return object
}

func TestCompatibilityVectors(t *testing.T) {
	for _, vector := range loadVectors(t) {
		vector := vector
		t.Run(vector.ID, func(t *testing.T) {
			message := newMessage(t, vector.Message)
			switch vector.Kind {
			case "json-decode":
				if err := DecodeJSON(vector.JSON, message); err != nil {
					t.Fatalf("decode: %v", err)
				}
				object := jsonObject(t, message)
				for key, expected := range vector.Expect {
					if !reflect.DeepEqual(object[key], expected) {
						t.Fatalf("%s: got %v want %v", key, object[key], expected)
					}
				}
			case "json-reject-unknown-enum":
				var failure error
				if err := DecodeJSON(vector.JSON, message); err != nil {
					failure = err
				}
				if failure == nil {
					t.Fatal("unknown enum name must be rejected")
				}
			case "json-enum-number-validation":
				err := DecodeJSON(vector.JSON, message)
				if err == nil {
					t.Fatal("unknown enum number must be rejected")
				}
				var unknown *UnknownEnumError
				if !errors.As(err, &unknown) {
					t.Fatalf("want UnknownEnumError, got %T: %v", err, err)
				}
			case "oneof-required":
				if err := DecodeJSON(vector.JSON, message); err != nil {
					t.Fatalf("decode: %v", err)
				}
				err := RequireOneof(message, "payload")
				var missing *MissingOneofError
				if !errors.As(err, &missing) {
					t.Fatalf("want MissingOneofError, got %v", err)
				}
			case "binary-decode":
				bytes, err := hex.DecodeString(vector.Hex)
				if err != nil {
					t.Fatal(err)
				}
				if err := proto.Unmarshal(bytes, message); err != nil {
					t.Fatalf("binary decode: %v", err)
				}
				object := jsonObject(t, message)
				for key, expected := range vector.Expect {
					if !reflect.DeepEqual(object[key], expected) {
						t.Fatalf("%s: got %v want %v", key, object[key], expected)
					}
				}
			case "json-int64-string":
				if err := DecodeJSON(vector.JSON, message); err != nil {
					t.Fatalf("decode: %v", err)
				}
				request, ok := message.(*HostRequest)
				if !ok {
					t.Fatalf("unexpected message %T", message)
				}
				if request.GetExpiresAtUnixMs() != 9007199254740993 {
					t.Fatalf("int64 lost precision: %d", request.GetExpiresAtUnixMs())
				}
			case "json-zero-omission":
				if err := DecodeJSON(vector.JSON, message); err != nil {
					t.Fatalf("decode: %v", err)
				}
				if object := jsonObject(t, message); !reflect.DeepEqual(
					object,
					map[string]any{"requestId": "req-6"},
				) {
					t.Fatalf("zero values must be omitted, got %v", object)
				}
			default:
				t.Fatalf("unsupported vector kind %s", vector.Kind)
			}
		})
	}
}

// The stock protojson decoder rejects every unknown field, which is why Access
// must use DecodeJSON instead. This documents the observed failure mode.
func TestStockProtoJSONRejectsAdditiveFields(t *testing.T) {
	vector := vectorByID(t, "json-unknown-field-is-ignored")
	var request HostRequest
	if err := protojson.Unmarshal(vector.JSON, &request); err == nil {
		t.Fatal("stock protojson is expected to reject unknown fields")
	}
}

func TestRequireOneofRejectsAnUnknownOneofName(t *testing.T) {
	err := RequireOneof(&RequestPayload{}, "not_a_oneof")
	if err == nil {
		t.Fatal("unknown oneof name must fail")
	}
	var missing *MissingOneofError
	if errors.As(err, &missing) {
		t.Fatal("an unknown oneof name is not a missing variant")
	}
}
