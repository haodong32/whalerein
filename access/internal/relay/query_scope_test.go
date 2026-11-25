package relay

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/auth"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

type fakeQueryScopes struct {
	sessionID string
	calls     []string
	err       error
}

func (f *fakeQueryScopes) AuthorizeObjectReserve(_ context.Context, hostID, deviceID, channelID, requestID, connectionRef string, enrollment uint64, expiresAtUnixMs int64) (string, error) {
	f.calls = append(f.calls, requestID+"\x00"+connectionRef)
	return f.sessionID, f.err
}

func queryReserveRequest() ReserveRequest {
	request := validReserveRequest()
	request.SessionID = ""
	request.RequestID = "request-1"
	request.ConnectionRef = "0123456789abcdef:7"
	request.EnrollmentGeneration = 0
	return request
}

func TestRelayQueryReserveBindsTheAdmittedRequestAndConnection(t *testing.T) {
	service, _, metadata, payloads := newTestService(t, true)
	metadata.grant = testGrant()
	scopes := &fakeQueryScopes{sessionID: "login-session"}
	service.SetQueryScopes(scopes)

	reservation, err := service.Reserve(context.Background(), queryReserveRequest())
	if err != nil {
		t.Fatal(err)
	}
	if reservation.ObjectID == "" || len(scopes.calls) != 1 {
		t.Fatalf("reservation=%+v calls=%v", reservation, scopes.calls)
	}
	input := payloads.reserved[0]
	if input.RequestID != "request-1" || input.MessageID != "" {
		t.Fatalf("query parent identity = request %q message %q", input.RequestID, input.MessageID)
	}
	if input.AppSession != "login-session" || input.ConnectionRef != "0123456789abcdef:7" {
		t.Fatalf("connection ref = %q", input.ConnectionRef)
	}
	if input.EnrollmentGeneration == nil || *input.EnrollmentGeneration != 0 {
		t.Fatal("enrollment generation was not bound")
	}
}

func TestDurableCommandResultInheritsLoginFromCommand(t *testing.T) {
	service, _, metadata, payloads := newTestService(t, true)
	metadata.grant = testGrant()
	req := queryReserveRequest()
	req.ConnectionRef = ""
	grant := metadata.grant
	metadata.command = store.Request{RequestID: req.RequestID, Kind: whalereinv1.RequestKind_REQUEST_KIND_COMMAND,
		HostID: req.HostID, DeviceID: req.DeviceID, ChannelID: req.ChannelID, DeviceSessionID: "command-login",
		ExpiresAt: time.UnixMilli(req.ExpiresAtUnixMs), Stamp: &store.GrantStamp{GrantGeneration: grant.GrantGeneration,
			RevocationGeneration: grant.RevocationGeneration, DeviceRevocationGeneration: grant.DeviceRevocationGeneration}}
	if _, err := service.Reserve(context.Background(), req); err != nil {
		t.Fatal(err)
	}
	input := payloads.reserved[0]
	if input.AppSession != "command-login" || input.RequestID != req.RequestID || input.ConnectionRef != "" {
		t.Fatal("command parent scope was lost")
	}
}

func TestRelayQueryReserveRejectsForeignScope(t *testing.T) {
	cases := []struct {
		name   string
		mutate func(*ReserveRequest)
		scopes *fakeQueryScopes
		want   error
	}{
		{
			name:   "waiter authority denies",
			mutate: func(*ReserveRequest) {},
			scopes: &fakeQueryScopes{err: errors.New("no waiter")},
			want:   ErrForbidden,
		},
		{
			name: "foreign enrollment",
			mutate: func(request *ReserveRequest) {
				request.EnrollmentGeneration = 9
			},
			scopes: &fakeQueryScopes{},
			want:   ErrForbidden,
		},
		{
			name: "no admitted command for an unscoped result",
			mutate: func(request *ReserveRequest) {
				request.ConnectionRef = ""
			},
			scopes: &fakeQueryScopes{},
			want:   ErrForbidden,
		},
		{
			name: "invalid connection ref",
			mutate: func(request *ReserveRequest) {
				request.ConnectionRef = "not-a-ref"
			},
			scopes: &fakeQueryScopes{},
			want:   ErrInvalid,
		},
		{
			name: "query binding on an attachment part",
			mutate: func(request *ReserveRequest) {
				request.Purpose = whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_ATTACHMENT_PART
				request.PartID = "part-1"
			},
			scopes: &fakeQueryScopes{},
			want:   ErrInvalid,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			service, _, metadata, payloads := newTestService(t, true)
			metadata.grant = testGrant()
			service.SetQueryScopes(tc.scopes)
			request := queryReserveRequest()
			tc.mutate(&request)
			if _, err := service.Reserve(context.Background(), request); !errors.Is(err, tc.want) {
				t.Fatalf("reserve error = %v, want %v", err, tc.want)
			}
			if len(payloads.reserved) != 0 {
				t.Fatal("rejected query scope reserved ciphertext")
			}
		})
	}
}

func TestRelayFetchRequiresAnIssuedQueryObject(t *testing.T) {
	service, _, metadata, payloads := newTestService(t, true)
	metadata.grant = testGrant()
	object := testObject()
	object.RequestID = "request-1"
	object.MessageID = ""
	object.ConnectionRef = "0123456789abcdef:7"
	object.AppSession = ""

	metadata.objects[object.ObjectID] = object
	if _, err := service.Fetch(context.Background(), ObjectRequest{
		ObjectID: object.ObjectID, RelayVersion: Epoch, Signature: []byte("signed"),
	}); !errors.Is(err, ErrForbidden) {
		t.Fatalf("unissued query object fetch = %v", err)
	}
	if len(payloads.opened) != 0 {
		t.Fatal("unissued query object released bytes")
	}

	issuedAt := testNow.Add(-time.Second)
	object.IssuedAt = &issuedAt
	metadata.objects[object.ObjectID] = object
	if _, err := service.Fetch(context.Background(), ObjectRequest{
		ObjectID: object.ObjectID, RelayVersion: Epoch, Signature: []byte("signed"),
	}); err != nil {
		t.Fatalf("issued query object fetch: %v", err)
	}
	if len(payloads.opened) != 1 {
		t.Fatal("issued query object was not released")
	}
}

func TestRelayDeviceFetchRequiresItsOwnIssuedObject(t *testing.T) {
	service, authority, metadata, payloads := newTestService(t, true)
	metadata.grant = testGrant()
	object := testObject()
	object.RequestID = "request-1"
	object.MessageID = ""
	object.ConnectionRef = "0123456789abcdef:7"
	object.AppSession = "managed-login"
	issuedAt := testNow.Add(-time.Second)
	object.IssuedAt = &issuedAt
	metadata.objects[object.ObjectID] = object
	payloads.object = object

	// A foreign Device token never reads another Device's object.
	authority.identity = auth.Identity{DeviceID: "device-2"}
	if _, err := service.Fetch(context.Background(), ObjectRequest{
		ObjectID: object.ObjectID, RelayVersion: Epoch, BearerToken: "token",
	}); !errors.Is(err, ErrForbidden) {
		t.Fatalf("foreign device fetch = %v", err)
	}
	// The owner still needs an issued query object.
	unissued := object
	unissued.IssuedAt = nil
	metadata.objects[object.ObjectID] = unissued
	authority.identity = auth.Identity{DeviceID: "device-1", SessionID: object.AppSession}
	if _, err := service.Fetch(context.Background(), ObjectRequest{
		ObjectID: object.ObjectID, RelayVersion: Epoch, BearerToken: "token",
	}); !errors.Is(err, ErrForbidden) {
		t.Fatalf("unissued device fetch = %v", err)
	}
	metadata.objects[object.ObjectID] = object
	if _, err := service.Fetch(context.Background(), ObjectRequest{
		ObjectID: object.ObjectID, RelayVersion: Epoch, BearerToken: "token",
	}); err != nil {
		t.Fatalf("issued device fetch: %v", err)
	}
	if len(payloads.opened) != 1 {
		t.Fatalf("issued device object released %d times", len(payloads.opened))
	}
}

func TestRelayFinishRechecksTheLiveQueryScope(t *testing.T) {
	service, _, metadata, payloads := newTestService(t, true)
	metadata.grant = testGrant()
	enrollment := int64(0)
	object := testObject()
	object.RequestID = "request-1"
	object.MessageID = ""
	object.ConnectionRef = "0123456789abcdef:7"
	object.EnrollmentGeneration = &enrollment
	object.AppSession = ""
	metadata.objects[object.ObjectID] = object
	payloads.object = object

	scopes := &fakeQueryScopes{}
	service.SetQueryScopes(scopes)
	request := FinishRequest{
		ObjectRequest: ObjectRequest{
			ObjectID: object.ObjectID, RelayVersion: Epoch, Signature: []byte("signed"),
		},
		CiphertextBytes: uint64(object.Length),
		SHA256:          object.Digest,
	}
	if _, err := service.Finish(context.Background(), request); err != nil {
		t.Fatalf("finish with a live waiter: %v", err)
	}
	if len(scopes.calls) != 1 {
		t.Fatal("finish did not reauthorize the query scope")
	}

	scopes.err = errors.New("waiter expired")
	if _, err := service.Finish(context.Background(), request); !errors.Is(err, ErrForbidden) {
		t.Fatalf("finish after waiter loss = %v", err)
	}
}

func TestRelayCheckGrantFencesEnrollmentDrift(t *testing.T) {
	service, _, metadata, payloads := newTestService(t, true)
	metadata.grant = testGrant()
	object := testObject()
	drifted := int64(9)
	object.EnrollmentGeneration = &drifted
	metadata.objects[object.ObjectID] = object
	if _, err := service.Fetch(context.Background(), ObjectRequest{
		ObjectID: object.ObjectID, RelayVersion: Epoch, Signature: []byte("signed"),
	}); !errors.Is(err, ErrForbidden) {
		t.Fatalf("enrollment drift = %v", err)
	}
	if len(payloads.opened) != 0 {
		t.Fatal("enrollment-drifted object released bytes")
	}
}

func TestRelayQueryFinishRequiresTheFullBinding(t *testing.T) {
	service, _, metadata, payloads := newTestService(t, true)
	metadata.grant = testGrant()
	object := testObject()
	object.RequestID = "request-1"
	object.MessageID = ""
	metadata.objects[object.ObjectID] = object
	payloads.object = object
	request := FinishRequest{
		ObjectRequest: ObjectRequest{
			ObjectID: object.ObjectID, RelayVersion: Epoch, Signature: []byte("signed"),
		},
		CiphertextBytes: uint64(object.Length),
		SHA256:          object.Digest,
	}
	if _, err := service.Finish(context.Background(), request); !errors.Is(err, ErrForbidden) {
		t.Fatalf("unbound query finish = %v", err)
	}

	foreign := WrongOwnerError{Requested: "other"}
	if foreign.Error() == "" {
		t.Fatal("wrong-owner error has no text")
	}
}
