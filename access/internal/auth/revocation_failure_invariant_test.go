package auth

import (
	"context"
	"errors"
	"reflect"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

type failedGrantRevocation struct{ *fakeStore }

func (failedGrantRevocation) RevokeGrant(context.Context, string, string, time.Time) error {
	return errors.New("synthetic revocation persistence failure")
}

type replayMarkAttempt struct {
	host                    string
	devices                 []string
	revokedBefore, markedAt time.Time
	generations             map[string]uint64
}
type trackingRevocations struct {
	*fakeStore
	failure  error
	attempts []replayMarkAttempt
}

type trackingRevocationTransport struct {
	*fakeTransport
	hosts []string
}

func (t *trackingRevocationTransport) PublishAndAwait(ctx context.Context, host string, request *whalereinv1.HostRequest, timeout time.Duration) (*whalereinv1.HostResult, error) {
	t.hosts = append(t.hosts, host)
	return t.fakeTransport.PublishAndAwait(ctx, host, request, timeout)
}

func (s *trackingRevocations) MarkRevocationsReplayed(ctx context.Context, host string, devices []string, revokedBefore, markedAt time.Time, generations map[string]uint64) error {
	snapshot := replayMarkAttempt{host: host, devices: append([]string(nil), devices...), revokedBefore: revokedBefore, markedAt: markedAt, generations: map[string]uint64{}}
	for id, generation := range generations {
		snapshot.generations[id] = generation
	}
	s.attempts = append(s.attempts, snapshot)
	if s.failure != nil {
		return s.failure
	}
	return s.fakeStore.MarkRevocationsReplayed(ctx, host, devices, revokedBefore, markedAt, generations)
}

// Stubbed collaborators exercise service orchestration, not database transactions or network delivery.
func TestHostGrantRevocationDoesNotNotifyBeforeStoreSuccess(t *testing.T) {
	service, fs, presence, transport := newService(t)
	ctx := context.Background()
	fs.grants[grantKey("h1", "d1")] = true
	presence.online["h1"] = true
	if err := service.RevokeHostGrant(ctx, "missing", "d1"); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("missing grant error = %v", err)
	}
	if len(transport.disconnected) != 0 || len(transport.awaited) != 0 {
		t.Fatal("missing grant reached transport")
	}
	service.store = failedGrantRevocation{fs}
	if err := service.RevokeHostGrant(ctx, "h1", "d1"); err == nil {
		t.Fatal("revocation persistence failure was hidden")
	}
	if len(transport.disconnected) != 0 || len(transport.awaited) != 0 || !fs.grants[grantKey("h1", "d1")] {
		t.Fatal("failed revocation changed stub authority or reached transport")
	}
}

// A Broker enforcement failure leaves the durable revocation committed but
// keeps the operation retryable, and the Host notice is not claimed before the
// scoped fence is confirmed. A successful retry then delivers the notice.
func TestHostGrantEnforcementFailureKeepsTheScopedOperationRetryable(t *testing.T) {
	service, fs, _, presence, transport, enforcer := newServiceWithFakes(t)
	ctx := context.Background()
	fs.grants[grantKey("h1", "d1")] = true
	fs.pending = []store.Revocation{{HostID: "h1", DeviceID: "d1", GrantGeneration: 4, RevokedAt: time.Unix(10, 0)}}
	presence.online["h1"] = true
	transport.awaitFn = successAck
	enforcer.err = errors.New("synthetic broker failure")
	if err := service.RevokeHostGrant(ctx, "h1", "d1"); err == nil {
		t.Fatal("Broker enforcement failure was hidden")
	}
	if len(transport.awaited) != 0 {
		t.Fatal("Host notice was claimed before the Broker fence was confirmed")
	}
	if fs.grants[grantKey("h1", "d1")] {
		t.Fatal("durable revocation was rolled back by the Broker failure")
	}
	enforcer.err = nil
	if err := service.RevokeHostGrant(ctx, "h1", "d1"); err != nil {
		t.Fatal(err)
	}
	if len(transport.awaited) != 1 {
		t.Fatalf("retry did not deliver the Host notice: %d", len(transport.awaited))
	}
}

// A failed ACK mark remains pending in the stub and is attempted again on Host readiness.
func TestHostGrantNoticeMarkFailureRetainsPendingUntilReplay(t *testing.T) {
	service, fs, presence, transport := newService(t)
	ctx := context.Background()
	fs.grants[grantKey("h1", "d1")] = true
	fs.grants[grantKey("h2", "d1")] = true
	revokedAt, observedAt := time.Unix(10, 0), time.Unix(20, 0)
	fs.pending = []store.Revocation{{HostID: "h1", DeviceID: "d1", GrantGeneration: 4, RevokedAt: revokedAt}}
	sentinel := errors.New("synthetic acknowledgement persistence failure")
	tracked := &trackingRevocations{fakeStore: fs, failure: sentinel}
	trackedTransport := &trackingRevocationTransport{fakeTransport: transport}
	service.store = tracked
	service.transport = trackedTransport
	service.now = func() time.Time { return observedAt }
	presence.online["h1"] = true
	transport.awaitFn = successAck
	if err := service.RevokeHostGrant(ctx, "h1", "d1"); !errors.Is(err, sentinel) {
		t.Fatalf("mark error = %v", err)
	}
	expected := replayMarkAttempt{host: "h1", devices: []string{"d1"}, revokedBefore: revokedAt, markedAt: observedAt, generations: map[string]uint64{"d1": 4}}
	if len(tracked.attempts) != 1 || !reflect.DeepEqual(tracked.attempts[0], expected) {
		t.Fatal("incorrect initial scoped acknowledgement mark")
	}
	if len(fs.pending) != 1 || len(fs.markCalls) != 0 {
		t.Fatal("failed acknowledgement removed pending stub state")
	}
	if fs.grants[grantKey("h1", "d1")] || !fs.grants[grantKey("h2", "d1")] {
		t.Fatal("stub grant state changed unexpectedly")
	}
	if err := service.AuthorizeHost(ctx, Identity{DeviceID: "d1"}, "h1"); !errors.Is(err, ErrForbidden) {
		t.Fatal("failed mark restored stub authority")
	}
	tracked.failure = nil
	if err := service.ReplayPendingRevocations(ctx, "h1"); err != nil {
		t.Fatal(err)
	}
	if len(tracked.attempts) != 2 || !reflect.DeepEqual(tracked.attempts[1], expected) {
		t.Fatal("replay lost the scoped acknowledgement mark")
	}
	if len(fs.pending) != 0 || len(fs.markCalls) != 1 {
		t.Fatal("successful replay did not clear pending stub state")
	}
	if len(transport.awaited) != 3 {
		t.Fatal("expected initial, replay and final notices")
	}
	if !reflect.DeepEqual(trackedTransport.hosts, []string{"h1", "h1", "h1"}) {
		t.Fatal("notice transport host routing changed")
	}
	for _, request := range transport.awaited {
		if request.GetHostId() != "h1" {
			t.Fatal("notice envelope host changed")
		}
	}
	var initial, replay, final whalereinv1.RevocationNotice
	if err := proto.Unmarshal(transport.awaited[0].Body, &initial); err != nil {
		t.Fatal(err)
	}
	if err := proto.Unmarshal(transport.awaited[1].Body, &replay); err != nil {
		t.Fatal(err)
	}
	if err := proto.Unmarshal(transport.awaited[2].Body, &final); err != nil {
		t.Fatal(err)
	}
	if initial.Final || !reflect.DeepEqual(initial.DeviceIds, []string{"d1"}) || initial.GrantGenerations["d1"] != 4 || initial.RevokedAtUnixMs != revokedAt.UnixMilli() {
		t.Fatal("initial notice scope changed")
	}
	if replay.Final || !reflect.DeepEqual(replay.DeviceIds, []string{"d1"}) || replay.GrantGenerations["d1"] != 4 || replay.RevokedAtUnixMs != revokedAt.UnixMilli() {
		t.Fatal("replay or final notice scope changed")
	}
	if !final.Final || len(final.DeviceIds) != 0 || len(final.GrantGenerations) != 0 {
		t.Fatal("final notice must not include device IDs or grant generations")
	}
}
