package broker

import (
	"bytes"
	"context"
	"crypto/sha256"
	"errors"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/config"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

// recordingStager captures the exact staged request and returns a reference
// the relay can verify.
type recordingStager struct {
	mu        sync.Mutex
	requests  []StageRequest
	reference *whalereinv1.PayloadReference
	err       error
	release   chan struct{}
	started   chan struct{}
}

func (s *recordingStager) Stage(_ context.Context, request StageRequest) (*whalereinv1.PayloadReference, error) {
	s.mu.Lock()
	s.requests = append(s.requests, request)
	s.mu.Unlock()
	if s.started != nil {
		select {
		case s.started <- struct{}{}:
		default:
		}
	}
	if s.release != nil {
		<-s.release
	}
	if s.err != nil {
		return nil, s.err
	}
	if s.reference != nil {
		return s.reference, nil
	}
	digest := sha256Of(request.Ciphertext)
	return &whalereinv1.PayloadReference{
		ObjectId:        strings.Repeat("a", 32),
		OwnerNodeId:     "node-a",
		CiphertextBytes: uint64(len(request.Ciphertext)),
		Sha256:          digest,
		ExpiresAtUnixMs: time.Now().Add(time.Minute).UnixMilli(),
		Purpose:         request.Purpose,
	}, nil
}

func sha256Of(body []byte) []byte {
	sum := sha256.Sum256(body)
	return sum[:]
}

func TestEncodedPublishBytesMatchesTheWireFormula(t *testing.T) {
	cases := []struct {
		topic      string
		payload    int
		properties int
		want       int
	}{
		{"whalerein/v1/h/h1/request", 0, 0, encodedPublishBytes("whalerein/v1/h/h1/request", 0, 0)},
		{"t", 1, 0, 1 + 1 + (2 + 1 + 2 + 1 + 0 + 1)},
		{"t", 127, 0, 1 + 2 + (2 + 1 + 2 + 1 + 0 + 127)},
		{"t", 128, 0, 1 + 2 + (2 + 1 + 2 + 1 + 0 + 128)},
		{"t", 0, 300, 1 + 2 + (2 + 1 + 2 + 2 + 300 + 0)},
	}
	for _, tc := range cases {
		if got := encodedPublishBytes(tc.topic, tc.payload, tc.properties); got != tc.want {
			t.Fatalf("encoded(%q,%d,%d) = %d, want %d", tc.topic, tc.payload, tc.properties, got, tc.want)
		}
	}
	// The encoded size grows monotonically with the payload and never ignores
	// the topic or properties; an estimate from ciphertext bytes alone is
	// never the limit.
	if encodedPublishBytes("longer/topic", 10, 0) <= encodedPublishBytes("t", 10, 0) {
		t.Fatal("topic bytes were not counted")
	}
	if encodedPublishBytes("t", 10, 5) <= encodedPublishBytes("t", 10, 0) {
		t.Fatal("property bytes were not counted")
	}
}

func TestSelectPacketLimitTakesTheStrictestPositiveLimit(t *testing.T) {
	advertised := &config.TransportEffective{PacketBytes: 16 << 10}
	if got := selectPacketLimit(65536, 32768, advertised); got != 16<<10 {
		t.Fatalf("limit = %d", got)
	}
	if got := selectPacketLimit(65536, 32768, nil); got != 32768 {
		t.Fatalf("broker limit = %d", got)
	}
	if got := selectPacketLimit(65536, 0, nil); got != 65536 {
		t.Fatalf("local limit = %d", got)
	}
	if got := selectPacketLimit(65536, whalereinv1.BrokerPacketLimitBytes, &config.TransportEffective{}); got != 65536 {
		t.Fatalf("zero advertisement raised or lowered the limit: %d", got)
	}
}

func TestRelayEpochIsRequired(t *testing.T) {
	for _, version := range []uint32{0, 1, 2, 4} {
		err := validateRelayEpoch(version)
		if err == nil || !errors.Is(err, ErrRelayEpoch) {
			t.Fatalf("epoch %d accepted: %v", version, err)
		}
		if !errors.Is(err, ErrPeerTransportUnsupported) {
			t.Fatalf("epoch %d is not a transport incompatibility: %v", version, err)
		}
	}
	if err := validateRelayEpoch(RelayEpoch); err != nil {
		t.Fatalf("epoch 3 rejected: %v", err)
	}
}

// relayTestSession builds one session with an explicit packet limit and no
// receiver advertisement so the local/broker hop limit is deterministic.
func relayTestSession(t *testing.T, packetBytes uint32, stager PayloadStager) (*pahoSession, *recordingPublish) {
	t.Helper()
	local := config.DefaultTransportEffective()
	local.PacketBytes = packetBytes
	local.EnvelopeBytes = 16 << 20
	recorder := &recordingPublish{}
	session := newPublisher(recorder.publish, local, nil)
	return &pahoSession{
		publisher:   session,
		peers:       nil,
		budget:      local,
		brokerLimit: uint64(packetBytes),
		receivers:   NewReceiverBudgets(),
		stager:      &stagerRef{stager: stager},
	}, recorder
}

type recordingPublish struct {
	mu       sync.Mutex
	topics   []string
	payloads [][]byte
}

func (r *recordingPublish) publish(_ context.Context, topic string, payload []byte) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.topics = append(r.topics, topic)
	r.payloads = append(r.payloads, append([]byte(nil), payload...))
	return nil
}

func (r *recordingPublish) snapshot() ([]string, [][]byte) {
	r.mu.Lock()
	defer r.mu.Unlock()
	return append([]string(nil), r.topics...), append([][]byte(nil), r.payloads...)
}

func TestPublishInlineForBelowAndAtTheLimitAndReferenceAbove(t *testing.T) {
	const limit = 512
	topic := RequestTopic("h1")
	makeRequest := func(body int) []byte {
		payload, err := proto.Marshal(&whalereinv1.HostRequest{RequestId: "r1", HostId: "h1", Kind: whalereinv1.RequestKind_REQUEST_KIND_QUERY, Body: bytes.Repeat([]byte("x"), body)})
		if err != nil {
			t.Fatal(err)
		}
		return payload
	}
	// Find the largest fitting body and, when the protobuf length prefix does
	// not skip it, the body whose complete packet is exactly at the boundary.
	fit, exact := -1, -1
	for body := 0; body < 4096; body++ {
		encoded := encodedPublishBytes(topic, len(makeRequest(body)), 0)
		if encoded == limit {
			exact, fit = body, body
			break
		}
		if encoded > limit {
			break
		}
		fit = body
	}
	if fit < 1 {
		t.Fatalf("boundary search failed: fit=%d exact=%d", fit, exact)
	}
	atBody := fit
	if exact >= 0 {
		atBody = exact
		if got := encodedPublishBytes(topic, len(makeRequest(atBody)), 0); got != limit {
			t.Fatalf("at-boundary packet = %d", got)
		}
	}
	belowBody := atBody - 1
	if got := encodedPublishBytes(topic, len(makeRequest(belowBody)), 0); got > limit {
		t.Fatalf("below-boundary packet = %d", got)
	}

	session, recorder := relayTestSession(t, limit, nil)
	if err := session.Publish(context.Background(), topic, makeRequest(belowBody), 1); err != nil {
		t.Fatalf("below boundary: %v", err)
	}
	session, atRecorder := relayTestSession(t, limit, nil)
	if err := session.Publish(context.Background(), topic, makeRequest(atBody), 1); err != nil {
		t.Fatalf("at boundary: %v", err)
	}
	// Above the boundary without a stager: explicit failure before publish.
	above := makeRequest(atBody + 64)
	if encodedPublishBytes(topic, len(above), 0) <= limit {
		t.Fatal("above-boundary fixture still fits")
	}
	if err := session.Publish(context.Background(), topic, above, 1); !errors.Is(err, ErrStagingUnavailable) {
		t.Fatalf("above boundary without staging = %v", err)
	}
	if topics, _ := recorder.snapshot(); len(topics) != 1 {
		t.Fatalf("below-boundary publication = %d", len(topics))
	}
	if topics, _ := atRecorder.snapshot(); len(topics) != 1 {
		t.Fatalf("at-boundary publication = %d", len(topics))
	}
}

func TestOversizedPayloadIsReplacedByAPacketSizedReference(t *testing.T) {
	session, recorder := relayTestSession(t, 512, &recordingStager{})
	topic := DeviceEventTopic("h1", "d1")
	event := &whalereinv1.EventEnvelope{
		HostId: "h1", DeviceId: "d1", ChannelId: "c1", Nonce: []byte("nonce"),
		Ciphertext: bytes.Repeat([]byte("ciphertext"), 64), RelayVersion: RelayEpoch,
	}
	payload, _ := proto.Marshal(event)
	if err := session.Publish(context.Background(), topic, payload, 1); err != nil {
		t.Fatalf("reference publish: %v", err)
	}
	topics, payloads := recorder.snapshot()
	if len(topics) != 1 || topics[0] != topic {
		t.Fatalf("topics = %v", topics)
	}
	var sent whalereinv1.EventEnvelope
	if err := proto.Unmarshal(payloads[0], &sent); err != nil {
		t.Fatal(err)
	}
	if len(sent.Ciphertext) != 0 {
		t.Fatal("inline ciphertext survived the reference rewrite")
	}
	if sent.PayloadReference == nil || !bytes.Equal(sent.Nonce, event.Nonce) {
		t.Fatalf("reference/nonce = %+v/%x", sent.PayloadReference, sent.Nonce)
	}
	if sent.PayloadReference.CiphertextBytes != uint64(len(event.Ciphertext)) {
		t.Fatalf("reference length = %d", sent.PayloadReference.CiphertextBytes)
	}
}

func TestReferenceIsPublishedOnlyAfterStagingCompletes(t *testing.T) {
	stager := &recordingStager{release: make(chan struct{}), started: make(chan struct{}, 1)}
	session, recorder := relayTestSession(t, 512, stager)
	topic := DeviceEventTopic("h1", "d1")
	payload, _ := proto.Marshal(&whalereinv1.EventEnvelope{
		HostId: "h1", DeviceId: "d1", ChannelId: "c1", Nonce: []byte("n"),
		Ciphertext: bytes.Repeat([]byte("x"), 2048), RelayVersion: RelayEpoch,
	})
	done := make(chan error, 1)
	go func() { done <- session.Publish(context.Background(), topic, payload, 1) }()
	select {
	case <-stager.started:
	case <-time.After(time.Second):
		t.Fatal("staging did not start")
	}
	if topics, _ := recorder.snapshot(); len(topics) != 0 {
		t.Fatal("reference published before staging completed")
	}
	close(stager.release)
	select {
	case err := <-done:
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(time.Second):
		t.Fatal("publish did not finish")
	}
	if topics, _ := recorder.snapshot(); len(topics) != 1 {
		t.Fatalf("publication = %d", len(topics))
	}
	if len(stager.requests) != 1 || len(stager.requests[0].Ciphertext) != 2048 {
		t.Fatalf("staged request = %+v", stager.requests)
	}
}

func TestReferenceThatCannotFitFailsBeforePublish(t *testing.T) {
	// A tiny limit cannot fit any reference envelope, so the relay must fail
	// explicitly without publishing a partial packet.
	session, recorder := relayTestSession(t, 32, &recordingStager{})
	topic := DeviceEventTopic("h1", "d1")
	payload, _ := proto.Marshal(&whalereinv1.EventEnvelope{
		HostId: "h1", DeviceId: "d1", ChannelId: "c1", Ciphertext: []byte("x"), RelayVersion: RelayEpoch,
	})
	err := session.Publish(context.Background(), topic, payload, 1)
	if !errors.Is(err, ErrMessageBudget) {
		t.Fatalf("reference limit error = %v", err)
	}
	if topics, _ := recorder.snapshot(); len(topics) != 0 {
		t.Fatal("oversized reference was published")
	}
}

func TestEnvelopeBudgetIsEnforcedSeparately(t *testing.T) {
	session, recorder := relayTestSession(t, 4096, &recordingStager{})
	session.budget.EnvelopeBytes = 64
	topic := RequestTopic("h1")
	payload, _ := proto.Marshal(&whalereinv1.HostRequest{RequestId: "r1", HostId: "h1", Body: bytes.Repeat([]byte("x"), 256)})
	err := session.Publish(context.Background(), topic, payload, 1)
	if !errors.Is(err, ErrMessageBudget) {
		t.Fatalf("envelope error = %v", err)
	}
	var transportErr *TransportError
	if !errors.As(err, &transportErr) || transportErr.Code != transportCodeEnvelope {
		t.Fatalf("envelope code = %+v", err)
	}
	if topics, _ := recorder.snapshot(); len(topics) != 0 {
		t.Fatal("oversized envelope was published")
	}
}

func TestStagerFailureAndMismatchNeverPublish(t *testing.T) {
	topic := DeviceEventTopic("h1", "d1")
	payload, _ := proto.Marshal(&whalereinv1.EventEnvelope{
		HostId: "h1", DeviceId: "d1", ChannelId: "c1", Ciphertext: bytes.Repeat([]byte("x"), 2048), RelayVersion: RelayEpoch,
	})
	for name, stager := range map[string]*recordingStager{
		"stager error": {err: errors.New("quota")},
		"bad length": {reference: &whalereinv1.PayloadReference{
			ObjectId: "a", OwnerNodeId: "n", CiphertextBytes: 1,
			Sha256: sha256Of([]byte("x")), ExpiresAtUnixMs: time.Now().Add(time.Minute).UnixMilli(),
			Purpose: whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_EVENT,
		}},
		"missing expiry": {reference: &whalereinv1.PayloadReference{
			ObjectId: "a", OwnerNodeId: "n", CiphertextBytes: 2048,
			Sha256:  sha256Of(bytes.Repeat([]byte("x"), 2048)),
			Purpose: whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_EVENT,
		}},
	} {
		t.Run(name, func(t *testing.T) {
			session, recorder := relayTestSession(t, 512, stager)
			if err := session.Publish(context.Background(), topic, payload, 1); err == nil {
				t.Fatal("invalid reference accepted")
			}
			if topics, _ := recorder.snapshot(); len(topics) != 0 {
				t.Fatal("invalid reference was published")
			}
		})
	}
}

func TestAlreadyReferencedEnvelopeIsNotRestamped(t *testing.T) {
	topic := DeviceEventTopic("h1", "d1")
	reference := &whalereinv1.PayloadReference{
		ObjectId: "b", OwnerNodeId: "n", CiphertextBytes: 2048,
		Sha256: sha256Of(bytes.Repeat([]byte("x"), 2048)), ExpiresAtUnixMs: time.Now().Add(time.Minute).UnixMilli(),
		Purpose: whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_EVENT,
	}
	payload, _ := proto.Marshal(&whalereinv1.EventEnvelope{
		HostId: "h1", DeviceId: "d1", ChannelId: "c1", Nonce: []byte("n"),
		PayloadReference: reference, RelayVersion: RelayEpoch,
	})
	stager := &recordingStager{err: errors.New("must not stage a reference")}
	session, recorder := relayTestSession(t, 512, stager)
	if err := session.Publish(context.Background(), topic, payload, 1); err != nil {
		t.Fatal(err)
	}
	_, payloads := recorder.snapshot()
	if len(payloads) != 1 || !bytes.Equal(payloads[0], payload) {
		t.Fatal("already-referenced envelope was rewritten")
	}
	if len(stager.requests) != 0 {
		t.Fatal("reference was restaged")
	}
}
