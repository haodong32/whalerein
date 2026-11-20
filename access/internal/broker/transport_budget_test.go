package broker

import (
	"context"
	"crypto/sha256"
	"errors"
	"fmt"
	"math"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/config"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

func profileBudget(t *testing.T, name string) config.TransportEffective {
	t.Helper()
	effective, err := (config.Config{Transport: config.TransportConfig{Profile: name}}).ResolveTransport()
	if err != nil {
		t.Fatalf("resolve %s: %v", name, err)
	}
	return effective
}

func defaultAdvertisement(t *testing.T, generation uint64) *whalereinv1.ReceiveAdvertisement {
	t.Helper()
	values, ok := whalereinv1.TransportProfileValuesFor("default")
	if !ok {
		t.Fatal("default profile missing")
	}
	return &whalereinv1.ReceiveAdvertisement{
		Profile: whalereinv1.TransportProfile_TRANSPORT_PROFILE_DEFAULT,
		Budget: &whalereinv1.TransportBudget{
			PacketBytes: values.PacketBytes, FragmentPayloadBytes: values.FragmentPayloadBytes,
			FragmentCount: values.FragmentCount, EnvelopeBytes: values.EnvelopeBytes,
			BulkConcurrency: values.BulkConcurrency, ReservedBytes: values.ReservedBytes,
			FragmentIdleTimeoutMs: values.FragmentIdleTimeoutMs, TransferTimeoutMs: values.TransferTimeoutMs,
		},
		ConnectionGeneration: generation,
	}
}

// overrideAdvertisement keeps the default base profile but advertises an
// asymmetric, relation-valid operator override: a smaller envelope and a
// narrower fragment width than the packaged default.
func overrideAdvertisement(t *testing.T, generation uint64, mutate func(*whalereinv1.TransportBudget)) *whalereinv1.ReceiveAdvertisement {
	t.Helper()
	advertisement := defaultAdvertisement(t, generation)
	advertisement.Budget.EnvelopeBytes = 1 << 20
	advertisement.Budget.FragmentPayloadBytes = 32 << 10
	advertisement.Budget.FragmentCount = 32
	advertisement.Budget.BulkConcurrency = 2
	advertisement.Budget.ReservedBytes = 1 << 20
	advertisement.Budget.TransferTimeoutMs = 60_000
	if mutate != nil {
		mutate(advertisement.Budget)
	}
	return advertisement
}

func TestAdvertisementValidationEnforcesRelationsNotProfileEquality(t *testing.T) {
	effective, generation, err := validateAdvertisement(defaultAdvertisement(t, 4))
	if err != nil || generation != 4 || effective.Profile != "default" || effective.EnvelopeBytes != 16<<20 {
		t.Fatalf("valid advertisement = %+v/%d/%v", effective, generation, err)
	}
	// A relation-valid override is accepted and its advertised values are
	// returned; the base label only supplies the profile vocabulary and the
	// packet framing allowance that is not carried on the wire.
	overridden, generation, err := validateAdvertisement(overrideAdvertisement(t, 9, nil))
	if err != nil {
		t.Fatalf("relation-valid override rejected: %v", err)
	}
	if generation != 9 || overridden.Profile != "default" ||
		overridden.WireProfile != whalereinv1.TransportProfile_TRANSPORT_PROFILE_DEFAULT {
		t.Fatalf("override scope = %+v/%d", overridden, generation)
	}
	if overridden.EnvelopeBytes != 1<<20 || overridden.FragmentPayloadBytes != 32<<10 ||
		overridden.FragmentCount != 32 || overridden.BulkConcurrency != 2 ||
		overridden.ReservedBytes != 1<<20 || overridden.PacketOverheadBytes != 4096 ||
		overridden.TransferTimeout != time.Minute {
		t.Fatalf("override was not taken as advertised: %+v", overridden)
	}
	if _, _, err := validateAdvertisement(nil); !errors.Is(err, ErrPeerBudgetMissing) {
		t.Fatalf("missing advertisement = %v", err)
	}
	missingBudget := defaultAdvertisement(t, 1)
	missingBudget.Budget = nil
	if _, _, err := validateAdvertisement(missingBudget); !errors.Is(err, ErrPeerBudgetMissing) {
		t.Fatalf("missing budget = %v", err)
	}
	zeroGeneration := defaultAdvertisement(t, 0)
	if _, _, err := validateAdvertisement(zeroGeneration); !errors.Is(err, ErrPeerBudgetInvalid) {
		t.Fatalf("zero generation = %v", err)
	}
	unknownProfile := defaultAdvertisement(t, 1)
	unknownProfile.Profile = whalereinv1.TransportProfile(99)
	if _, _, err := validateAdvertisement(unknownProfile); !errors.Is(err, ErrPeerBudgetInvalid) {
		t.Fatalf("unknown profile = %v", err)
	}
	unspecified := defaultAdvertisement(t, 1)
	unspecified.Profile = whalereinv1.TransportProfile_TRANSPORT_PROFILE_UNSPECIFIED
	if _, _, err := validateAdvertisement(unspecified); !errors.Is(err, ErrPeerBudgetInvalid) {
		t.Fatalf("unspecified profile = %v", err)
	}
}

func TestAdvertisementValidationRejectsContradictoryOverrides(t *testing.T) {
	for name, mutate := range map[string]func(*whalereinv1.TransportBudget){
		"fragment count below the envelope": func(b *whalereinv1.TransportBudget) { b.FragmentCount = 31 },
		"reserved below one envelope":       func(b *whalereinv1.TransportBudget) { b.ReservedBytes = (1 << 20) - 1 },
		"reserved below in-flight payloads": func(b *whalereinv1.TransportBudget) { b.BulkConcurrency = 64 },
		"packet does not fit fragment+overhead": func(b *whalereinv1.TransportBudget) {
			b.FragmentPayloadBytes = 65_000
		},
		"idle not below transfer": func(b *whalereinv1.TransportBudget) {
			b.FragmentIdleTimeoutMs = 60_000
		},
		"zero packet":           func(b *whalereinv1.TransportBudget) { b.PacketBytes = 0 },
		"zero fragment width":   func(b *whalereinv1.TransportBudget) { b.FragmentPayloadBytes = 0 },
		"zero fragment count":   func(b *whalereinv1.TransportBudget) { b.FragmentCount = 0 },
		"zero envelope":         func(b *whalereinv1.TransportBudget) { b.EnvelopeBytes = 0 },
		"zero concurrency":      func(b *whalereinv1.TransportBudget) { b.BulkConcurrency = 0 },
		"zero reserved":         func(b *whalereinv1.TransportBudget) { b.ReservedBytes = 0 },
		"zero idle timeout":     func(b *whalereinv1.TransportBudget) { b.FragmentIdleTimeoutMs = 0 },
		"zero transfer timeout": func(b *whalereinv1.TransportBudget) { b.TransferTimeoutMs = 0 },
	} {
		t.Run(name, func(t *testing.T) {
			advertisement := overrideAdvertisement(t, 1, mutate)
			if _, _, err := validateAdvertisement(advertisement); !errors.Is(err, ErrPeerBudgetInvalid) {
				t.Fatalf("contradictory advertisement = %v", err)
			}
		})
	}
}

func TestAdvertisementValidationRejectsUnrepresentableEnvelope(t *testing.T) {
	// Every relation holds, but a TransportFrame total is a uint32, so this
	// envelope could never be transferred.
	advertisement := overrideAdvertisement(t, 1, func(b *whalereinv1.TransportBudget) {
		b.EnvelopeBytes = 1 << 32
		b.FragmentCount = 1 << 17 // ceil(2^32 / 32768)
		b.ReservedBytes = 1 << 32
	})
	if _, _, err := validateAdvertisement(advertisement); !errors.Is(err, ErrPeerBudgetInvalid) {
		t.Fatalf("unrepresentable envelope = %v", err)
	}
}

func TestHostReadyRegistersAndSendsWithinOverriddenAdvertisement(t *testing.T) {
	session := newFakeSession()
	service := testService(t, &fakeConnector{sessions: []*fakeSession{session}})
	connectedAt := time.Now().UTC()
	service.handleNotification(&Notification{Sequence: 1, Event: "connected", Kind: "host", HostID: "h1", ConnID: 1, AtUnixMilli: connectedAt.UnixMilli()})
	body, err := proto.Marshal(&whalereinv1.HostReady{
		HostId: "h1", ConnectedAtUnixMs: connectedAt.UnixMilli(), TransportVersion: 2,
		Receive: overrideAdvertisement(t, 1, nil),
	})
	if err != nil {
		t.Fatal(err)
	}
	service.handleReady(&whalereinv1.HostResult{
		HostId: "h1", Status: whalereinv1.OnlineStatus_succeeded, ResultBody: body,
	})
	budget, ok := service.Receivers().Lookup(RequestTopic("h1"))
	if !ok || budget.EnvelopeBytes != 1<<20 || budget.FragmentPayloadBytes != 32<<10 || budget.FragmentCount != 32 {
		t.Fatalf("override not registered: %+v/%v", budget, ok)
	}
	// The sender uses the registered override: a payload the local default
	// envelope would accept fails against the Host's smaller advertised one.
	transport := newLogicalTransport(func(context.Context, string, []byte) error { return nil }, func(string, []byte) {})
	if err := transport.sendTo(context.Background(), RequestTopic("h1"), make([]byte, (1<<20)+1), "ack", true, &budget); !errors.Is(err, ErrMessageBudget) {
		t.Fatalf("override envelope not enforced: %v", err)
	}
	if negotiated := negotiateBudget(transport.budget(), budget); negotiated.FragmentPayloadBytes != 32<<10 || negotiated.FragmentCount != 32 {
		t.Fatalf("override not negotiated: %+v", negotiated)
	}
}

func TestReceiverBudgetsBindGenerationAndTakeSharedMinimum(t *testing.T) {
	registry := NewReceiverBudgets()
	host := receiverRoute{kind: "host", hostID: "h1"}
	device := receiverRoute{kind: "device", hostID: "h1", deviceID: "d1"}
	registry.NoteConnection(host, 5, true)
	if !registry.Register(host, 5, 1, profileBudget(t, "default")) {
		t.Fatal("first advertisement rejected")
	}
	if registry.Register(host, 5, 1, profileBudget(t, "compact")) {
		t.Fatal("replayed generation refreshed the budget")
	}
	if registry.Register(host, 0, 9, profileBudget(t, "compact")) {
		t.Fatal("unknown connection refreshed the budget")
	}
	if !registry.Register(host, 5, 2, profileBudget(t, "default")) {
		t.Fatal("newer generation rejected")
	}
	registry.NoteConnection(device, 9, true)
	if !registry.Register(device, 9, 1, profileBudget(t, "compact")) {
		t.Fatal("device advertisement rejected")
	}
	selected, ok := registry.minimum([]receiverRoute{host, device})
	if !ok || selected.Profile != "compact" || selected.EnvelopeBytes != 8<<20 {
		t.Fatalf("shared minimum = %+v/%v", selected, ok)
	}
	if budget, ok := registry.Lookup(DeviceEventTopic("h1", "d1")); !ok || budget.Profile != "compact" {
		t.Fatalf("device lookup = %+v/%v", budget, ok)
	}

	// Reconnect invalidates the prior entry and a delayed advertisement from
	// the superseded connection is rejected.
	registry.NoteConnection(host, 6, true)
	if _, ok := registry.Lookup(RequestTopic("h1")); ok {
		t.Fatal("reconnect retained the prior advertisement")
	}
	if registry.Register(host, 5, 3, profileBudget(t, "roomy")) {
		t.Fatal("superseded connection refreshed the budget")
	}
	if !registry.Register(host, 6, 1, profileBudget(t, "roomy")) {
		t.Fatal("current connection advertisement rejected")
	}
	registry.NoteConnection(device, 9, false)
	if _, ok := registry.Lookup(DeviceEventTopic("h1", "d1")); ok {
		t.Fatal("disconnect retained the device advertisement")
	}
	registry.Reset()
	if registry.Count() != 0 {
		t.Fatal("reset retained entries")
	}
}

func TestFrameWidthIsConsistentAndWithinLocalBudget(t *testing.T) {
	receiver := newLogicalTransport(func(context.Context, string, []byte) error { return nil }, func(string, []byte) {})
	digest := sha256.Sum256(make([]byte, 10))
	makeFrame := func(width, total, count, index uint32, payload []byte) []byte {
		return encodeTransport(&whalereinv1.TransportFrame{
			Version: transportVersion, TransferId: "aabbccddeeff00112233445566778899", Topic: "data",
			Index: index, Count: count, TotalBytes: total, Digest: digest[:], Payload: payload,
			FragmentPayloadBytes: width, Profile: whalereinv1.TransportProfile_TRANSPORT_PROFILE_DEFAULT,
		})
	}
	if _, err := receiver.receive("data", makeFrame(0, 10, 1, 0, make([]byte, 10)), "ack"); transportCodeOf(err) != transportCodeWidth {
		t.Fatalf("missing width = %v", err)
	}
	if _, err := receiver.receive("data", makeFrame(65535, 10, 1, 0, make([]byte, 10)), "ack"); transportCodeOf(err) != transportCodeWidth {
		t.Fatalf("oversize width = %v", err)
	}
	if _, err := receiver.receive("data", makeFrame(5, 10, 2, 0, make([]byte, 5)), "ack"); err != nil {
		t.Fatal(err)
	}
	// A changed width for the same transfer is inconsistent geometry and never
	// reaches a second part reservation.
	if _, err := receiver.receive("data", makeFrame(10, 10, 2, 1, make([]byte, 5)), "ack"); transportCodeOf(err) != transportCodeGeometry {
		t.Fatalf("width change = %v", err)
	}
	// Count must be ceil(total/width) at the frame's own width.
	if _, err := receiver.receive("data", makeFrame(5, 10, 4, 0, make([]byte, 5)), "ack"); transportCodeOf(err) != transportCodeGeometry {
		t.Fatalf("geometry = %v", err)
	}
}

func TestTinyFragmentCannotBypassReassemblyAccounting(t *testing.T) {
	metrics := diagnostics.NewMetrics()
	receiver := newLogicalTransport(func(context.Context, string, []byte) error { return nil }, func(string, []byte) {})
	receiver.metrics = metrics
	digest := sha256.Sum256([]byte{0, 1})
	frame := func(id byte) []byte {
		return encodeTransport(&whalereinv1.TransportFrame{
			Version: transportVersion, TransferId: fmt.Sprintf("%032x", int(id)+1), Topic: "data", Index: 0, Count: 2,
			TotalBytes: 2, Digest: digest[:], Payload: []byte{id},
			FragmentPayloadBytes: 1, Profile: whalereinv1.TransportProfile_TRANSPORT_PROFILE_DEFAULT,
		})
	}
	concurrency := int(receiver.budget().BulkConcurrency)
	for index := 0; index < concurrency; index++ {
		if _, err := receiver.receive("data", frame(byte(index)), "ack"); err != nil {
			t.Fatalf("tiny fragment %d rejected: %v (code %s)", index, err, transportCodeOf(err))
		}
	}
	if _, err := receiver.receive("data", frame(0xee), "ack"); transportCodeOf(err) != transportCodeReassembly {
		t.Fatalf("tiny fragment bypassed admission: %v", err)
	}
	receiver.mu.Lock()
	reserved := receiver.reservedLocked()
	transfers := len(receiver.incoming)
	receiver.mu.Unlock()
	if transfers != concurrency || reserved <= int64(transfers*2) {
		t.Fatalf("bookkeeping not reserved: transfers=%d reserved=%d", transfers, reserved)
	}

	// A frame claiming a huge envelope at width 1 must fail geometry before any
	// part buffer is allocated.
	huge := encodeTransport(&whalereinv1.TransportFrame{
		Version: transportVersion, TransferId: "ffeeddccbbaa99887766554433221100", Topic: "data",
		Index: 0, Count: 274, TotalBytes: math.MaxUint32, Digest: digest[:], Payload: []byte{1, 2},
		FragmentPayloadBytes: 1, Profile: whalereinv1.TransportProfile_TRANSPORT_PROFILE_DEFAULT,
	})
	if _, err := receiver.receive("data", huge, "ack"); err == nil {
		t.Fatal("tiny-width huge envelope accepted")
	}
	if reserved := transferReservation(math.MaxUint64, math.MaxUint64, math.MaxUint64); reserved != math.MaxInt64 {
		t.Fatalf("reservation overflow = %d", reserved)
	}
	if budget := admissionBudget(receiver.budget()); budget <= int64(receiver.budget().ReservedBytes) {
		t.Fatal("derived admission budget did not account for fragment bookkeeping")
	}
}

func TestControlTrafficProgressesWhileReassemblyIsSaturated(t *testing.T) {
	receiver := newLogicalTransport(func(context.Context, string, []byte) error { return nil }, func(string, []byte) {})
	digest := sha256.Sum256([]byte{0, 1})
	for index := 0; index < int(receiver.budget().BulkConcurrency); index++ {
		frame := encodeTransport(&whalereinv1.TransportFrame{
			Version: transportVersion, TransferId: fmt.Sprintf("%032x", index+1), Topic: "data",
			Index: 0, Count: 2, TotalBytes: 2, Digest: digest[:], Payload: []byte{byte(index)},
			FragmentPayloadBytes: 1, Profile: whalereinv1.TransportProfile_TRANSPORT_PROFILE_DEFAULT,
		})
		if _, err := receiver.receive("data", frame, "ack"); err != nil {
			t.Fatal(err)
		}
	}
	// Small control envelopes are not framed and must not enter the reassembly
	// admission lane.
	if consumed, err := receiver.receive("data", []byte("legacy-control"), "ack"); consumed || err != nil {
		t.Fatalf("legacy control blocked: consumed=%v err=%v", consumed, err)
	}
	var published int
	transport := newLogicalTransport(func(_ context.Context, _ string, payload []byte) error {
		published++
		return nil
	}, func(string, []byte) {})
	if err := transport.send(context.Background(), "data", []byte("control"), "ack", false); err != nil {
		t.Fatalf("control send blocked: %v", err)
	}
	if published != 1 {
		t.Fatalf("control publish = %d", published)
	}
}

func TestInboundReservationsExpireAfterIdleTimeout(t *testing.T) {
	receiver := newLogicalTransport(func(context.Context, string, []byte) error { return nil }, func(string, []byte) {})
	receiver.local.FragmentIdleTimeout = 20 * time.Millisecond
	receiver.metrics = diagnostics.NewMetrics()
	digest := sha256.Sum256([]byte{0, 1})
	frame := encodeTransport(&whalereinv1.TransportFrame{
		Version: transportVersion, TransferId: "aabbccddeeff00112233445566778899", Topic: "data",
		Index: 0, Count: 2, TotalBytes: 2, Digest: digest[:], Payload: []byte{0},
		FragmentPayloadBytes: 1, Profile: whalereinv1.TransportProfile_TRANSPORT_PROFILE_DEFAULT,
	})
	if _, err := receiver.receive("data", frame, "ack"); err != nil {
		t.Fatal(err)
	}
	receiver.mu.Lock()
	before := len(receiver.incoming)
	receiver.mu.Unlock()
	if before != 1 {
		t.Fatalf("incoming = %d", before)
	}
	receiver.now = func() time.Time { return time.Now().Add(time.Hour) }
	receiver.expire()
	receiver.mu.Lock()
	after := len(receiver.incoming)
	reserved := receiver.reservedLocked()
	receiver.mu.Unlock()
	if after != 0 || reserved != 0 {
		t.Fatalf("stalled transfer did not release: transfers=%d reserved=%d", after, reserved)
	}
}

func TestSendRequiresRegisteredPeerBudgetAndUsesNegotiatedWidth(t *testing.T) {
	transport := newLogicalTransport(func(context.Context, string, []byte) error { return nil }, func(string, []byte) {})
	if err := transport.sendTo(context.Background(), "data", make([]byte, transportChunkBytes+1), "ack", true, nil); !errors.Is(err, ErrPeerBudgetMissing) {
		t.Fatalf("missing peer budget = %v", err)
	}
	legacy := 0
	transport.publish = func(context.Context, string, []byte) error { legacy++; return nil }
	if err := transport.sendTo(context.Background(), "data", make([]byte, 1024), "ack", true, nil); err != nil || legacy != 1 {
		t.Fatalf("bootstrap envelope blocked: err=%v publishes=%d", err, legacy)
	}
	compact := profileBudget(t, "compact")
	if err := transport.sendTo(context.Background(), "data", make([]byte, compact.EnvelopeBytes+1), "ack", true, &compact); !errors.Is(err, ErrMessageBudget) {
		t.Fatalf("peer envelope bypassed = %v", err)
	}

	var sender *logicalTransport
	var delivered int
	var widths []uint32
	receiver := newLogicalTransport(func(_ context.Context, topic string, payload []byte) error {
		frame := &whalereinv1.TransportFrame{}
		if err := proto.Unmarshal(payload[len(transportPrefix):], frame); err != nil {
			return err
		}
		widths = append(widths, frame.GetFragmentPayloadBytes())
		_, err := sender.receive(topic, payload, "")
		return err
	}, func(string, []byte) { delivered++ })
	receiver.local = compact
	sender = newLogicalTransport(func(_ context.Context, topic string, payload []byte) error {
		_, err := receiver.receive(topic, payload, "ack")
		return err
	}, func(string, []byte) {})
	sender.ackTimeout = 100 * time.Millisecond
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	if err := sender.sendTo(ctx, "data", make([]byte, 1<<20), "ack", true, &compact); err != nil {
		t.Fatalf("negotiated send: %v", err)
	}
	deadline := time.Now().Add(time.Second)
	for delivered != 1 && time.Now().Before(deadline) {
		time.Sleep(time.Millisecond)
	}
	if delivered != 1 {
		t.Fatalf("delivered = %d", delivered)
	}
	for _, width := range widths {
		if width != compact.FragmentPayloadBytes {
			t.Fatalf("frame width = %d, want %d", width, compact.FragmentPayloadBytes)
		}
	}
}

func TestHostReadyAdvertisementIsBoundToTheCurrentConnection(t *testing.T) {
	session := newFakeSession()
	service := testService(t, &fakeConnector{sessions: []*fakeSession{session}})
	connectedAt := time.Now().UTC()
	service.handleNotification(&Notification{Sequence: 1, Event: "connected", Kind: "host", HostID: "h1", ConnID: 1, AtUnixMilli: connectedAt.UnixMilli()})
	ready := func(at time.Time, generation uint64) *whalereinv1.HostResult {
		body, err := proto.Marshal(&whalereinv1.HostReady{
			HostId: "h1", ConnectedAtUnixMs: at.UnixMilli(), TransportVersion: 2,
			Receive: defaultAdvertisement(t, generation),
		})
		if err != nil {
			t.Fatal(err)
		}
		return &whalereinv1.HostResult{HostId: "h1", Status: whalereinv1.OnlineStatus_succeeded, ResultBody: body}
	}
	service.handleReady(ready(connectedAt, 1))
	if _, ok := service.Receivers().Lookup(RequestTopic("h1")); !ok {
		t.Fatal("advertisement not registered")
	}
	// A newer broker connection invalidates the prior entry. A delayed proof
	// from the superseded connection cannot refresh it.
	service.handleNotification(&Notification{Sequence: 2, Event: "connected", Kind: "host", HostID: "h1", ConnID: 2, AtUnixMilli: connectedAt.Add(5 * time.Second).UnixMilli()})
	if _, ok := service.Receivers().Lookup(RequestTopic("h1")); ok {
		t.Fatal("reconnect retained the prior advertisement")
	}
	service.handleReady(ready(connectedAt, 2))
	if _, ok := service.Receivers().Lookup(RequestTopic("h1")); ok {
		t.Fatal("stale readiness refreshed the new connection")
	}
	service.handleReady(ready(connectedAt.Add(5*time.Second), 3))
	if budget, ok := service.Receivers().Lookup(RequestTopic("h1")); !ok || budget.Profile != "default" {
		t.Fatalf("current readiness not registered: %+v/%v", budget, ok)
	}
}
