package whalereinv1

import (
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
