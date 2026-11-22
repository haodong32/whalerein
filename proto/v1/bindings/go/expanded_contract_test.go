package whalereinv1

import (
	"bytes"
	"encoding/hex"
	"encoding/json"
	"errors"
	"os"
	"testing"

	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
)

// Expanded conversation, history, attachment and transport fixtures. The same
// vectors are checked by the Node and Rust peers; this test proves the
// committed Go binding keeps every new typed family and its binary shape.
func TestExpandedContractVectorsRoundTrip(t *testing.T) {
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
		"typed-reasoning-live":               func() proto.Message { return &EventPayload{} },
		"typed-reasoning-history":            func() proto.Message { return &HistoryPage{} },
		"typed-tool-activity":                func() proto.Message { return &HistoryPage{} },
		"typed-notice":                       func() proto.Message { return &HistoryPage{} },
		"typed-context-message":              func() proto.Message { return &HistoryPage{} },
		"typed-send-reply-committed":         func() proto.Message { return &ResponsePayload{} },
		"typed-send-reply-input":             func() proto.Message { return &ResponsePayload{} },
		"typed-history-page-read":            func() proto.Message { return &RequestPayload{} },
		"typed-history-exact-read":           func() proto.Message { return &RequestPayload{} },
		"typed-attachment-open":              func() proto.Message { return &RequestPayload{} },
		"typed-attachment-metadata":          func() proto.Message { return &ResponsePayload{} },
		"typed-attachment-part":              func() proto.Message { return &ResponsePayload{} },
		"typed-attachment-close":             func() proto.Message { return &RequestPayload{} },
		"typed-request-transport":            func() proto.Message { return &HostRequest{} },
		"typed-receive-advertisement":        func() proto.Message { return &HostReady{} },
		"typed-payload-reference":            func() proto.Message { return &PayloadReference{} },
		"typed-source-capabilities-expanded": func() proto.Message { return &SourceCapabilities{} },
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
		t.Fatalf("checked %d expanded vectors, want %d", seen, len(selected))
	}
}

func TestExpandedRequestAndContentPayloadsRequireOneKnownVariant(t *testing.T) {
	for name, message := range map[string]proto.Message{
		"RequestPayload":  &RequestPayload{},
		"ResponsePayload": &ResponsePayload{},
		"ContentBlock":    &ContentBlock{},
	} {
		oneof := "payload"
		if name == "ContentBlock" {
			oneof = "content"
		}
		err := RequireOneof(message, oneof)
		var missing *MissingOneofError
		if !errors.As(err, &missing) {
			t.Fatalf("%s: want MissingOneofError, got %v", name, err)
		}
	}
	open := &RequestPayload{Payload: &RequestPayload_OpenAttachment{OpenAttachment: &OpenAttachment{
		Session:       &SessionKey{HostId: "h", SessionId: "s"},
		AttachmentRef: "occurrence-1",
	}}}
	if err := RequireOneof(open, "payload"); err != nil {
		t.Fatalf("open attachment oneof: %v", err)
	}
	if open.GetOpenAttachment().GetAttachmentRef() != "occurrence-1" {
		t.Fatal("attachment occurrence identity lost")
	}
	reasoning := &ContentBlock{
		BlockId: "b",
		Content: &ContentBlock_Reasoning{Reasoning: &ReasoningContent{Text: "think"}},
	}
	if err := RequireOneof(reasoning, "content"); err != nil {
		t.Fatalf("reasoning oneof: %v", err)
	}
	if reasoning.GetReasoning().GetText() != "think" {
		t.Fatal("reasoning content lost")
	}
}
