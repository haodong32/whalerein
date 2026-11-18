package whalereinv1

import (
	"bytes"
	"encoding/hex"
	"encoding/json"
	"os"
	"testing"

	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
)

func TestProtoJSONAndBinaryAreTheSameMessage(t *testing.T) {
	jsonData, err := os.ReadFile("../../fixtures/host_request.json")
	if err != nil {
		t.Fatal(err)
	}
	var request HostRequest
	if err := protojson.Unmarshal(jsonData, &request); err != nil {
		t.Fatalf("unmarshal json: %v", err)
	}
	binary, err := proto.Marshal(&request)
	if err != nil {
		t.Fatal(err)
	}
	want, err := os.ReadFile("../../fixtures/host_request.bin")
	if err != nil {
		t.Fatal(err)
	}
	if string(binary) != string(want) {
		t.Fatalf("binary mismatch: got %x want %x", binary, want)
	}
	encoded, err := protojson.Marshal(&request)
	if err != nil {
		t.Fatal(err)
	}
	var roundtrip HostRequest
	if err := protojson.Unmarshal(encoded, &roundtrip); err != nil {
		t.Fatal(err)
	}
	if !proto.Equal(&request, &roundtrip) {
		t.Fatal("json roundtrip changed the message")
	}
}

// The observation contract is additive inside protocol version 1. These
// vectors prove that the Go reader keeps the question stamp, final-page marker
// and explicit interests distinct from an empty or defaulted collection.
func TestQuestionObservationVectorsRoundTrip(t *testing.T) {
	raw, err := os.ReadFile("../../../../tests/fixtures/proto/messages.json")
	if err != nil {
		t.Fatal(err)
	}
	var fixture struct {
		Messages []struct {
			CaseID string          `json:"caseId"`
			Name   string          `json:"name"`
			JSON   json.RawMessage `json:"json"`
			Hex    string          `json:"hex"`
		} `json:"messages"`
	}
	if err := json.Unmarshal(raw, &fixture); err != nil {
		t.Fatal(err)
	}
	selected := map[string]func() proto.Message{
		"typed-question-descriptor":              func() proto.Message { return &QuestionDescriptor{} },
		"typed-question-descriptor-unavailable":  func() proto.Message { return &QuestionDescriptor{} },
		"typed-question-page":                    func() proto.Message { return &QuestionPage{} },
		"typed-question-page-unavailable-detail": func() proto.Message { return &QuestionPage{} },
		"typed-watch-interests":                  func() proto.Message { return &WatchSession{} },
		"typed-watch-questions-only":             func() proto.Message { return &WatchReady{} },
		"typed-session-state-questions":          func() proto.Message { return &SessionState{} },
	}
	seen := 0
	for _, entry := range fixture.Messages {
		factory, ok := selected[entry.CaseID]
		if !ok {
			continue
		}
		message := factory()
		if err := protojson.Unmarshal(entry.JSON, message); err != nil {
			t.Fatalf("%s: unmarshal json: %v", entry.CaseID, err)
		}
		binary, err := proto.Marshal(message)
		if err != nil {
			t.Fatalf("%s: marshal: %v", entry.CaseID, err)
		}
		want, err := hex.DecodeString(entry.Hex)
		if err != nil {
			t.Fatalf("%s: fixture hex: %v", entry.CaseID, err)
		}
		if !bytes.Equal(binary, want) {
			t.Fatalf("%s: binary mismatch", entry.CaseID)
		}
		var decoded proto.Message = factory()
		if err := proto.Unmarshal(want, decoded); err != nil {
			t.Fatalf("%s: unmarshal binary: %v", entry.CaseID, err)
		}
		encoded, err := protojson.Marshal(decoded)
		if err != nil {
			t.Fatalf("%s: marshal json: %v", entry.CaseID, err)
		}
		roundtrip := factory()
		if err := protojson.Unmarshal(encoded, roundtrip); err != nil {
			t.Fatalf("%s: unmarshal roundtrip: %v", entry.CaseID, err)
		}
		if !proto.Equal(decoded, roundtrip) {
			t.Fatalf("%s: json roundtrip changed the message", entry.CaseID)
		}
		seen++
	}
	if seen != len(selected) {
		t.Fatalf("checked %d observation vectors, want %d", seen, len(selected))
	}
}

func TestAvailableQuestionDescriptorRequiresStamp(t *testing.T) {
	descriptor := &QuestionDescriptor{Availability: QuestionAvailability_QUESTION_AVAILABILITY_AVAILABLE}
	if descriptor.GetStamp() != nil {
		t.Fatal("an available descriptor without a stamp must not decode as a stamped collection")
	}
	page := &QuestionPage{Batches: []*QuestionBatch{}}
	if page.GetComplete() || page.GetStamp() != nil {
		t.Fatal("an unstamped page must not claim a complete collection")
	}
}
