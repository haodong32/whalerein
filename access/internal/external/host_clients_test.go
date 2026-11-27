package external

import (
	"context"
	"errors"
	"reflect"
	"strconv"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

type hostClientsFixture struct {
	ids           []string
	online, known bool
	observe       func(*whalereinv1.HostRequest) (*whalereinv1.HostResult, error)
	calls         int
	scope         []string
	grantError    error
}

func (f *hostClientsFixture) HostClientIDs(_ context.Context, workspace, host, revision string) ([]string, error) {
	f.scope = []string{workspace, host, revision}
	return f.ids, f.grantError
}

func (f *hostClientsFixture) HostPresence(string) (bool, bool) { return f.online, f.known }

func (f *hostClientsFixture) PublishAndAwait(_ context.Context, _ string, request *whalereinv1.HostRequest, _ time.Duration) (*whalereinv1.HostResult, error) {
	f.calls++
	return f.observe(request)
}

func TestHostClientsUsesOnlyMatchingConfirmedGrantsAndCurrentHostEvidence(t *testing.T) {
	body, err := proto.Marshal(&whalereinv1.HostClients{ObservingDeviceIds: []string{"device-2", "device-foreign"}})
	if err != nil {
		t.Fatal(err)
	}
	result := func(request *whalereinv1.HostRequest) *whalereinv1.HostResult {
		if request.GetKind() != whalereinv1.RequestKind_REQUEST_KIND_HOST_CLIENTS || request.GetHostId() != "host-1" || request.GetExpiresAtUnixMs() == 0 {
			t.Fatalf("incorrect Host read: %v", request)
		}
		return &whalereinv1.HostResult{
			RequestId: request.GetRequestId(), HostId: request.GetHostId(),
			Status: whalereinv1.OnlineStatus_succeeded, ResultBody: body,
		}
	}
	for _, tc := range []struct {
		name, presence string
		online, known  bool
		observe        func(*whalereinv1.HostRequest) (*whalereinv1.HostResult, error)
		wantConnected  bool
	}{
		{"online", "known", true, true, func(req *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) { return result(req), nil }, true},
		{"paused", "known", false, true, nil, false},
		{"reconciling broker", "unavailable", false, false, nil, false},
		{"timeout", "unavailable", true, true, func(*whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
			return nil, context.DeadlineExceeded
		}, false},
		{"wrong Host", "unavailable", true, true, func(req *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
			answer := result(req)
			answer.HostId = "other-host"
			return answer, nil
		}, false},
		{"mismatched request id", "unavailable", true, true, func(req *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
			answer := result(req)
			answer.RequestId = "other-request"
			return answer, nil
		}, false},
		{"malformed payload", "unavailable", true, true, func(req *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
			answer := result(req)
			answer.ResultBody = []byte{0xff}
			return answer, nil
		}, false},
		{"over-limit observation", "unavailable", true, true, func(req *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
			ids := make([]string, 513)
			for index := range ids {
				ids[index] = "device-" + strconv.Itoa(index)
			}
			oversized, err := proto.Marshal(&whalereinv1.HostClients{ObservingDeviceIds: ids})
			if err != nil {
				t.Fatal(err)
			}
			answer := result(req)
			answer.ResultBody = oversized
			return answer, nil
		}, false},
		{"failed status", "unavailable", true, true, func(req *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
			answer := result(req)
			answer.Status = whalereinv1.OnlineStatus_failed
			return answer, nil
		}, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			f := &hostClientsFixture{ids: []string{"device-1", "device-2"}, online: tc.online, known: tc.known, observe: tc.observe}
			s := New(Trust{}, nil, "", WithHostClients(f, f, f))
			got, err := s.HostClients(context.Background(), "workspace-1", "host-1", "3")
			want := HostClientList{Presence: tc.presence, Devices: []HostClient{
				{DeviceID: "device-1"}, {DeviceID: "device-2", Connected: tc.wantConnected},
			}}
			if err != nil || !reflect.DeepEqual(got, want) || !reflect.DeepEqual(f.scope, []string{"workspace-1", "host-1", "3"}) {
				t.Fatalf("list=%+v err=%v scope=%v", got, err, f.scope)
			}
		})
	}
	f := &hostClientsFixture{grantError: store.ErrNotFound}
	s := New(Trust{}, nil, "", WithHostClients(f, f, f))
	if _, err := s.HostClients(context.Background(), "foreign", "host-1", "3"); !errors.Is(err, store.ErrNotFound) || f.calls != 0 {
		t.Fatalf("foreign workspace reached Host: err=%v calls=%d", err, f.calls)
	}
	f.grantError = nil
	got, err := s.HostClients(context.Background(), "workspace-1", "host-1", "3")
	if err != nil || got.Devices == nil || len(got.Devices) != 0 || got.Presence != "known" || f.calls != 0 {
		t.Fatalf("empty grant list = %+v, err=%v, probe calls=%d", got, err, f.calls)
	}
}

// An older Host that cannot report active client observations still yields the
// confirmed paired rows, but never fabricates a connected Device.
func TestHostClientsOlderHostWithoutObservationsKeepsPairedRowsUnconnected(t *testing.T) {
	f := &hostClientsFixture{
		ids: []string{"device-1", "device-2"}, online: true, known: true,
		observe: func(req *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
			return &whalereinv1.HostResult{
				RequestId: req.GetRequestId(), HostId: req.GetHostId(),
				Status: whalereinv1.OnlineStatus_succeeded,
			}, nil
		},
	}
	s := New(Trust{}, nil, "", WithHostClients(f, f, f))
	got, err := s.HostClients(context.Background(), "workspace", "host", "1")
	want := HostClientList{Presence: "known", Devices: []HostClient{
		{DeviceID: "device-1"}, {DeviceID: "device-2"},
	}}
	if err != nil || !reflect.DeepEqual(got, want) {
		t.Fatalf("older Host observation = %+v, err=%v", got, err)
	}
}

// A missing presence source (supported older Access wiring) withholds the live
// observation instead of presenting the paired rows as connected.
func TestHostClientsWithoutPresenceSourceReportsUnavailable(t *testing.T) {
	f := &hostClientsFixture{ids: []string{"device-1"}}
	s := New(Trust{}, nil, "", WithHostClients(f, f, nil))
	got, err := s.HostClients(context.Background(), "workspace", "host", "1")
	if err != nil || got.Presence != "unavailable" || len(got.Devices) != 1 || got.Devices[0].Connected {
		t.Fatalf("missing presence source = %+v, err=%v", got, err)
	}
}

func TestPublishAuthorityWaitsForHostAcceptance(t *testing.T) {
	var sent *whalereinv1.ScopedAuthority
	fixture := &hostClientsFixture{observe: func(request *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
		if request.GetKind() != whalereinv1.RequestKind_REQUEST_KIND_AUTHORITY || request.GetHostId() != "host-1" ||
			request.GetDeviceId() != "device-1" || request.GetRequestId() == "" || request.GetExpiresAtUnixMs() == 0 {
			t.Fatalf("authority request = %v", request)
		}
		sent = &whalereinv1.ScopedAuthority{}
		if err := proto.Unmarshal(request.GetBody(), sent); err != nil {
			t.Fatal(err)
		}
		return &whalereinv1.HostResult{
			RequestId: request.GetRequestId(), HostId: "host-1",
			Status: whalereinv1.OnlineStatus_succeeded,
		}, nil
	}}
	service := &Service{hostClientProbe: fixture, now: func() time.Time { return time.Unix(1, 0) }}
	permit := &whalereinv1.ScopedAuthority{AuthorityId: "authority-1", HostId: "host-1", DeviceId: "device-1", SessionId: "session-1"}
	if err := service.PublishAuthority(context.Background(), "host-1", permit); err != nil {
		t.Fatal(err)
	}
	if sent.GetAuthorityId() != "authority-1" || sent.GetSessionId() != "session-1" {
		t.Fatalf("sent authority = %#v", sent)
	}
	if err := service.PublishAuthority(context.Background(), "host-1", nil); !errors.Is(err, ErrEnforcementUnavailable) {
		t.Fatalf("nil authority = %v", err)
	}
}

type proofSessionStore struct {
	session store.DeviceSession
	err     error
}

func (s proofSessionStore) InstallDeviceSession(context.Context, store.DeviceSession) error {
	return nil
}
func (s proofSessionStore) RevokeDeviceSession(context.Context, string, int64) error { return nil }
func (s proofSessionStore) DeviceSessionByCredential(context.Context, []byte, time.Time) (store.DeviceSession, error) {
	return s.session, s.err
}
func (s proofSessionStore) DeviceSessionPairingStatus(context.Context, store.DeviceSession, string, string) (string, error) {
	return "", nil
}

func TestRegisterSessionProofChecksTheBearerSessionThenRelaysTheExactProof(t *testing.T) {
	now := time.Unix(1_700_000_000, 0)
	proof := &whalereinv1.SessionProof{
		SessionId: "session-1", HostId: "host-1", DeviceId: "device-1",
		ChannelId: "channel-1", TranscriptHash: make([]byte, 32),
		IssuedAtUnixMs: now.UnixMilli(), ExpiresAtUnixMs: now.Add(30 * time.Second).UnixMilli(),
		Signature: make([]byte, 64),
	}
	var relayed []whalereinv1.RequestKind
	fixture := &hostClientsFixture{observe: func(request *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
		relayed = append(relayed, request.GetKind())
		if request.GetHostId() != "host-1" {
			t.Fatalf("wrong Host relay: %v", request)
		}
		if request.GetKind() == whalereinv1.RequestKind_REQUEST_KIND_SESSION_PROOF {
			if request.GetChannelId() != "channel-1" {
				t.Fatalf("wrong proof channel: %v", request)
			}
			var got whalereinv1.SessionProof
			if err := proto.Unmarshal(request.GetBody(), &got); err != nil {
				t.Fatal(err)
			}
			if !proto.Equal(&got, proof) {
				t.Fatalf("relayed proof = %v", &got)
			}
		}
		return &whalereinv1.HostResult{
			RequestId: request.GetRequestId(), HostId: request.GetHostId(),
			Status: whalereinv1.OnlineStatus_succeeded,
		}, nil
	}}
	service := &Service{
		hostClientProbe: fixture, now: func() time.Time { return now },
		deviceSessions: proofSessionStore{session: store.DeviceSession{
			ID: "session-1", DeviceID: "device-1", ProofPublicKey: make([]byte, 32),
		}},
	}
	if err := service.RegisterSessionProof(context.Background(), "session-1", "credential", proof); err != nil {
		t.Fatal(err)
	}
	if fixture.calls != 1 || len(relayed) != 1 {
		t.Fatalf("relay calls = %d/%v", fixture.calls, relayed)
	}
	authority := &whalereinv1.ScopedAuthority{SessionId: "session-1", HostId: "host-1", DeviceId: "device-1"}
	if err := service.PublishAuthority(context.Background(), "host-1", authority); err != nil {
		t.Fatal(err)
	}
	if fixture.calls != 3 || len(relayed) != 3 ||
		relayed[1] != whalereinv1.RequestKind_REQUEST_KIND_AUTHORITY ||
		relayed[2] != whalereinv1.RequestKind_REQUEST_KIND_SESSION_PROOF {
		t.Fatalf("authority/proof retry = %d/%v", fixture.calls, relayed)
	}
	if err := service.RegisterSessionProof(context.Background(), "session-2", "credential", proof); !errors.Is(err, ErrAuthorizationMismatch) {
		t.Fatalf("wrong session = %v", err)
	}
	stale := proto.Clone(proof).(*whalereinv1.SessionProof)
	stale.ExpiresAtUnixMs = now.UnixMilli()
	if err := service.RegisterSessionProof(context.Background(), "session-1", "credential", stale); !errors.Is(err, ErrAuthorizationMismatch) {
		t.Fatalf("stale proof = %v", err)
	}
}
