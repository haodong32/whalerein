package relay

import (
	"bytes"
	"context"
	"crypto/sha256"
	"errors"
	"io"
	"strings"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/auth"
	"github.com/whalerein/whalerein/access/internal/payload"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

var testNow = time.UnixMilli(1_700_000_000_000)

func newFakes() (*fakeAuth, *fakeStore, *fakePayloads) {
	return &fakeAuth{}, &fakeStore{objects: map[string]store.PayloadObject{}}, &fakePayloads{}
}

func newTestService(t *testing.T, enabled bool) (*Service, *fakeAuth, *fakeStore, *fakePayloads) {
	t.Helper()
	authority, metadata, payloads := newFakes()
	service, err := New(Dependencies{
		Auth: authority, Store: metadata, Payloads: payloads,
		Config: Config{
			Enabled: enabled, NodeID: "node-1", OwnerID: "owner-1",
			Retention: 10 * time.Minute, UploadTimeout: 30 * time.Second, ReadTimeout: time.Minute,
		},
		Now: func() time.Time { return testNow },
	})
	if err != nil {
		t.Fatal(err)
	}
	return service, authority, metadata, payloads
}

func digestOf(body []byte) []byte {
	sum := sha256.Sum256(body)
	return sum[:]
}

func testGrant() store.AdmissionGrant {
	return store.AdmissionGrant{
		HostID: "host-1", DeviceID: "device-1", Active: true,
		GrantGeneration: 4, RevocationGeneration: 5, DeviceRevocationGeneration: 6,
	}
}

func generations(grant store.AdmissionGrant) (*int64, *int64, *int64) {
	a, b, c := int64(grant.GrantGeneration), int64(grant.RevocationGeneration), int64(grant.DeviceRevocationGeneration)
	return &a, &b, &c
}

func testObject() store.PayloadObject {
	grant, revoke, deviceRevoke := generations(testGrant())
	return store.PayloadObject{
		ObjectID: "0123456789abcdef0123456789abcdef", NodeID: "node-1", OwnerID: "owner-1",
		HostID: "host-1", DeviceID: "device-1", AppSession: "session-1",
		ChannelID: "channel-1", Purpose: "PAYLOAD_PURPOSE_RESULT", PartID: "",
		State: store.PayloadReady, Length: 4, Digest: digestOf([]byte("body")),
		ExpiresAt:       testNow.Add(time.Minute),
		GrantGeneration: grant, RevocationGeneration: revoke, DeviceRevocationGeneration: deviceRevoke,
	}
}

func validReserveRequest() ReserveRequest {
	return ReserveRequest{
		RelayVersion: Epoch, HostID: "host-1", DeviceID: "device-1",
		SessionID: "session-1", ChannelID: "channel-1",
		Purpose:         whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_RESULT,
		CiphertextBytes: 4, SHA256: digestOf([]byte("body")),
		ExpiresAtUnixMs: testNow.Add(2 * time.Minute).UnixMilli(),
		SignedAtUnixMs:  testNow.UnixMilli(),
		Signature:       []byte("signed"),
	}
}

func TestNewValidatesEnabledDependencies(t *testing.T) {
	if _, err := New(Dependencies{Config: Config{Enabled: true}}); err == nil {
		t.Fatal("expected missing seams to fail")
	}
	if _, err := New(Dependencies{
		Auth: &fakeAuth{}, Store: &fakeStore{}, Payloads: &fakePayloads{},
		Config: Config{Enabled: true, NodeID: "n", Retention: time.Minute},
	}); err != nil {
		t.Fatal(err)
	}
	service, err := New(Dependencies{Config: Config{Enabled: false}})
	if err != nil {
		t.Fatal(err)
	}
	if service.Enabled() {
		t.Fatal("disabled service reported enabled")
	}
	if _, err := service.Reserve(context.Background(), validReserveRequest()); !errors.Is(err, ErrDisabled) {
		t.Fatalf("disabled reserve: %v", err)
	}
}

func TestReserveHostBindsGrantAndExactScope(t *testing.T) {
	service, authority, metadata, payloads := newTestService(t, true)
	metadata.grant = testGrant()
	request := validReserveRequest()
	request.PartID = "part-1"
	request.Purpose = whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_ATTACHMENT_PART

	reservation, err := service.Reserve(context.Background(), request)
	if err != nil {
		t.Fatal(err)
	}
	if len(payloads.reserved) != 1 {
		t.Fatal("reserve was not admitted")
	}
	input := payloads.reserved[0]
	if input.HostID != "host-1" || input.DeviceID != "device-1" || input.AppSession != "session-1" ||
		input.ChannelID != "channel-1" || input.Purpose != "PAYLOAD_PURPOSE_ATTACHMENT_PART" || input.PartID != "part-1" {
		t.Fatalf("scope = %+v", input)
	}
	if input.Length != 4 || !bytes.Equal(input.Digest, request.SHA256) {
		t.Fatalf("length/digest = %d %x", input.Length, input.Digest)
	}
	if input.GrantGeneration == nil || *input.GrantGeneration != 4 ||
		input.RevocationGeneration == nil || *input.RevocationGeneration != 5 ||
		input.DeviceRevocationGeneration == nil || *input.DeviceRevocationGeneration != 6 {
		t.Fatal("grant revisions were not bound")
	}
	if input.MessageID != input.ObjectID {
		t.Fatal("relay reservation must record its opaque object id as the only parent identity")
	}
	if reservation.ObjectID != input.ObjectID || reservation.OwnerNodeID != "owner-1" ||
		reservation.UploadPath != "/v1/relay/objects/"+input.ObjectID ||
		reservation.MaxBytes != 4 || reservation.ExpiresAtUnixMs != request.ExpiresAtUnixMs ||
		reservation.UploadTimeoutMS != 30_000 {
		t.Fatalf("reservation = %+v", reservation)
	}
	if len(authority.purposes) != 1 || authority.purposes[0] != auth.RelayAuthReserve {
		t.Fatalf("proof purposes = %v", authority.purposes)
	}
}

func TestReserveHonoursUploadPathPrefix(t *testing.T) {
	service, _, metadata, _ := newTestService(t, true)
	service.config.UploadPathPrefix = "/node/a/v1/relay/objects/"
	metadata.grant = testGrant()
	reservation, err := service.Reserve(context.Background(), validReserveRequest())
	if err != nil {
		t.Fatal(err)
	}
	if !strings.HasPrefix(reservation.UploadPath, "/node/a/v1/relay/objects/") {
		t.Fatalf("upload path = %q", reservation.UploadPath)
	}
}

func TestReserveDevicePath(t *testing.T) {
	service, authority, metadata, payloads := newTestService(t, true)
	metadata.grant = testGrant()
	authority.identity = auth.Identity{DeviceID: "device-1", SessionID: "session-1"}
	request := validReserveRequest()
	request.Signature = nil
	request.BearerToken = "device-token"
	if _, err := service.Reserve(context.Background(), request); err != nil {
		t.Fatal(err)
	}
	if len(authority.purposes) != 0 {
		t.Fatal("device reserve used a host proof")
	}
	if len(payloads.reserved) != 1 {
		t.Fatal("reserve was not admitted")
	}

	// A Device cannot reserve for another Device.
	request.DeviceID = "device-2"
	if _, err := service.Reserve(context.Background(), request); !errors.Is(err, ErrForbidden) {
		t.Fatalf("foreign device: %v", err)
	}
	// A Device cannot claim a different App session.
	request.DeviceID = "device-1"
	request.SessionID = "other-session"
	if _, err := service.Reserve(context.Background(), request); !errors.Is(err, ErrForbidden) {
		t.Fatalf("foreign session: %v", err)
	}
	// An unauthorized host fails closed.
	request.SessionID = "session-1"
	authority.authorizeErr = auth.ErrForbidden
	if _, err := service.Reserve(context.Background(), request); !errors.Is(err, ErrForbidden) {
		t.Fatalf("authorize host: %v", err)
	}
	authority.authorizeErr = &auth.AuthorityDenial{Reason: auth.ReasonAuthorityUnavailable, StatusCode: 503}
	if _, err := service.Reserve(context.Background(), request); !errors.Is(err, ErrUnavailable) {
		t.Fatalf("authority unavailable: %v", err)
	}
}

func TestReserveRejectsInvalidScopeAndExpiry(t *testing.T) {
	cases := []struct {
		name   string
		mutate func(*ReserveRequest)
	}{
		{"epoch", func(r *ReserveRequest) { r.RelayVersion = 2 }},
		{"missing host", func(r *ReserveRequest) { r.HostID = "" }},
		{"host separator", func(r *ReserveRequest) { r.HostID = "a|b" }},
		{"missing device", func(r *ReserveRequest) { r.DeviceID = "" }},
		{"missing channel", func(r *ReserveRequest) { r.ChannelID = "" }},
		{"zero length", func(r *ReserveRequest) { r.CiphertextBytes = 0 }},
		{"huge length", func(r *ReserveRequest) { r.CiphertextBytes = 1 << 63 }},
		{"short digest", func(r *ReserveRequest) { r.SHA256 = r.SHA256[:31] }},
		{"missing expiry", func(r *ReserveRequest) { r.ExpiresAtUnixMs = 0 }},
		{"past expiry", func(r *ReserveRequest) { r.ExpiresAtUnixMs = testNow.UnixMilli() }},
		{"beyond retention", func(r *ReserveRequest) { r.ExpiresAtUnixMs = testNow.Add(time.Hour).UnixMilli() }},
		{"unsupported purpose", func(r *ReserveRequest) { r.Purpose = 0 }},
		{"unknown purpose number", func(r *ReserveRequest) { r.Purpose = 99 }},
		{"attachment without part", func(r *ReserveRequest) {
			r.Purpose = whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_ATTACHMENT_PART
		}},
		{"control in session", func(r *ReserveRequest) { r.SessionID = "a\nb" }},
		{"control in part", func(r *ReserveRequest) { r.PartID = "a\x00b" }},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			service, _, metadata, payloads := newTestService(t, true)
			metadata.grant = testGrant()
			request := validReserveRequest()
			test.mutate(&request)
			_, err := service.Reserve(context.Background(), request)
			if err == nil {
				t.Fatal("expected failure")
			}
			if len(payloads.reserved) != 0 {
				t.Fatal("invalid request reached storage")
			}
			if test.name == "epoch" {
				if !errors.Is(err, ErrEpoch) {
					t.Fatalf("epoch: %v", err)
				}
				return
			}
			if !errors.Is(err, ErrInvalid) {
				t.Fatalf("err = %v", err)
			}
		})
	}
}

func TestReserveRequiresActiveGrant(t *testing.T) {
	service, _, metadata, payloads := newTestService(t, true)
	metadata.grantErr = store.ErrNotFound
	if _, err := service.Reserve(context.Background(), validReserveRequest()); !errors.Is(err, ErrForbidden) {
		t.Fatalf("missing grant: %v", err)
	}
	metadata.grantErr = nil
	grant := testGrant()
	grant.Active = false
	metadata.grant = grant
	if _, err := service.Reserve(context.Background(), validReserveRequest()); !errors.Is(err, ErrForbidden) {
		t.Fatalf("inactive grant: %v", err)
	}
	grant = testGrant()
	grant.DeviceID = "device-2"
	metadata.grant = grant
	if _, err := service.Reserve(context.Background(), validReserveRequest()); !errors.Is(err, ErrForbidden) {
		t.Fatalf("grant principal mismatch: %v", err)
	}
	metadata.grant = testGrant()
	metadata.grantErr = nil
	payloads.reserveErr = payload.ErrQuota
	if _, err := service.Reserve(context.Background(), validReserveRequest()); !errors.Is(err, ErrQuota) {
		t.Fatalf("quota translation: %v", err)
	}
}

func TestReserveWrongOwnerDoesNotTouchStorage(t *testing.T) {
	service, _, metadata, payloads := newTestService(t, true)
	metadata.grant = testGrant()
	request := validReserveRequest()
	request.OwnerNode = "node-2"
	_, err := service.Reserve(context.Background(), request)
	if !errors.Is(err, ErrWrongOwner) {
		t.Fatalf("err = %v", err)
	}
	var wrong *WrongOwnerError
	if !errors.As(err, &wrong) || wrong.Requested != "node-2" {
		t.Fatalf("wrong owner detail = %+v", err)
	}
	if len(payloads.reserved) != 0 {
		t.Fatal("wrong owner reached storage")
	}
	request.OwnerNode = "owner-1"
	if _, err := service.Reserve(context.Background(), request); err != nil {
		t.Fatal(err)
	}
}

func TestReserveRejectsBadDeviceTokenAndProof(t *testing.T) {
	service, authority, metadata, _ := newTestService(t, true)
	metadata.grant = testGrant()
	request := validReserveRequest()
	request.Signature = nil
	request.BearerToken = "bad"
	authority.authErr = auth.ErrUnauthorized
	if _, err := service.Reserve(context.Background(), request); !errors.Is(err, ErrUnauthorized) {
		t.Fatalf("bad token: %v", err)
	}

	request = validReserveRequest()
	request.Signature = nil
	if _, err := service.Reserve(context.Background(), request); !errors.Is(err, ErrUnauthorized) {
		t.Fatalf("missing proof: %v", err)
	}
	request.Signature = []byte("signed")
	authority.proofErr = auth.ErrSignatureExpired
	if _, err := service.Reserve(context.Background(), request); !errors.Is(err, ErrUnauthorized) {
		t.Fatalf("expired signature: %v", err)
	}
	authority.proofErr = auth.ErrBadRequest
	if _, err := service.Reserve(context.Background(), request); !errors.Is(err, ErrInvalid) {
		t.Fatalf("bad fields: %v", err)
	}
}

func TestObjectRoutesRejectSharedBadInput(t *testing.T) {
	service, _, metadata, payloads := newTestService(t, true)
	metadata.grant = testGrant()
	metadata.objects[testObject().ObjectID] = testObject()
	objectID := testObject().ObjectID
	cases := []struct {
		name string
		req  ObjectRequest
		want error
	}{
		{"epoch", ObjectRequest{ObjectID: objectID, RelayVersion: 2, BearerToken: "t"}, ErrEpoch},
		{"object id", ObjectRequest{ObjectID: "ABC", RelayVersion: Epoch, BearerToken: "t"}, ErrInvalid},
		{"no auth", ObjectRequest{ObjectID: objectID, RelayVersion: Epoch}, ErrUnauthorized},
		{"unknown object", ObjectRequest{ObjectID: strings.Repeat("f", 32), RelayVersion: Epoch, BearerToken: "t"}, ErrNotFound},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			_, err := service.Fetch(context.Background(), test.req)
			if !errors.Is(err, test.want) {
				t.Fatalf("err = %v, want %v", err, test.want)
			}
		})
	}
	if len(payloads.opened) != 0 {
		t.Fatal("invalid fetch opened an object")
	}
}

func TestFetchRechecksGrantAndScope(t *testing.T) {
	service, authority, metadata, payloads := newTestService(t, true)
	metadata.grant = testGrant()
	object := testObject()
	metadata.objects[object.ObjectID] = object
	payloads.object = payload.Object{
		ObjectID: object.ObjectID, OwnerID: object.OwnerID, Length: object.Length,
		Digest: object.Digest, ExpiresAt: object.ExpiresAt, Purpose: "PAYLOAD_PURPOSE_RESULT",
	}
	authority.identity = auth.Identity{DeviceID: "device-1", SessionID: "session-1"}

	result, err := service.Fetch(context.Background(), ObjectRequest{
		ObjectID: object.ObjectID, RelayVersion: Epoch, BearerToken: "device-token",
	})
	if err != nil {
		t.Fatal(err)
	}
	defer result.Body.Close()
	body, _ := io.ReadAll(result.Body)
	if string(body) != "body" || result.Length != 4 || result.ExpiresAtUnixMs != object.ExpiresAt.UnixMilli() {
		t.Fatalf("fetch result = %q %d", body, result.Length)
	}
	if len(payloads.opened) != 1 {
		t.Fatal("fetch did not open once")
	}

	// Revision mismatch fences the old reference.
	metadata.grant = testGrant()
	metadata.grant.RevocationGeneration++
	if _, err := service.Fetch(context.Background(), ObjectRequest{
		ObjectID: object.ObjectID, RelayVersion: Epoch, BearerToken: "device-token",
	}); !errors.Is(err, ErrForbidden) {
		t.Fatalf("revision mismatch: %v", err)
	}
	metadata.grant = testGrant()

	// Foreign device and foreign session fail closed without opening.
	authority.identity = auth.Identity{DeviceID: "device-2", SessionID: "session-1"}
	if _, err := service.Fetch(context.Background(), ObjectRequest{
		ObjectID: object.ObjectID, RelayVersion: Epoch, BearerToken: "device-token",
	}); !errors.Is(err, ErrForbidden) {
		t.Fatalf("foreign device: %v", err)
	}
	authority.identity = auth.Identity{DeviceID: "device-1", SessionID: "session-2"}
	if _, err := service.Fetch(context.Background(), ObjectRequest{
		ObjectID: object.ObjectID, RelayVersion: Epoch, BearerToken: "device-token",
	}); !errors.Is(err, ErrForbidden) {
		t.Fatalf("foreign session: %v", err)
	}
	authority.identity = auth.Identity{DeviceID: "device-1", SessionID: "session-1"}
	authority.authorizeErr = auth.ErrForbidden
	if _, err := service.Fetch(context.Background(), ObjectRequest{
		ObjectID: object.ObjectID, RelayVersion: Epoch, BearerToken: "device-token",
	}); !errors.Is(err, ErrForbidden) {
		t.Fatalf("revoked grant: %v", err)
	}
	if len(payloads.opened) != 1 {
		t.Fatal("denied fetch released bytes")
	}
}

func TestFetchAllowsHostOriginatedObjectWithoutAppSession(t *testing.T) {
	service, authority, metadata, payloads := newTestService(t, true)
	metadata.grant = testGrant()
	// A Host uploads a result before it can know the Access-managed App
	// session. The object stays bound to the Device, channel, grant and the
	// sealed channel, so a session-bound reader of that Device may resolve it.
	authority.identity = auth.Identity{DeviceID: "device-1", SessionID: "session-2"}
	object := testObject()
	object.AppSession = ""
	metadata.objects[object.ObjectID] = object
	result, err := service.Fetch(context.Background(), ObjectRequest{
		ObjectID: object.ObjectID, RelayVersion: Epoch, BearerToken: "device-token",
	})
	if err != nil {
		t.Fatalf("host-originated fetch: %v", err)
	}
	_ = result.Body.Close()
	if len(payloads.opened) != 1 {
		t.Fatalf("expected one opened object, got %d", len(payloads.opened))
	}
}

func TestFetchExpiredAndMissingRevisionBindings(t *testing.T) {
	service, authority, metadata, payloads := newTestService(t, true)
	metadata.grant = testGrant()
	authority.identity = auth.Identity{DeviceID: "device-1", SessionID: "session-1"}
	object := testObject()
	object.ExpiresAt = testNow
	metadata.objects[object.ObjectID] = object
	if _, err := service.Fetch(context.Background(), ObjectRequest{
		ObjectID: object.ObjectID, RelayVersion: Epoch, BearerToken: "token",
	}); !errors.Is(err, ErrExpired) {
		t.Fatalf("expired: %v", err)
	}

	object = testObject()
	object.GrantGeneration = nil
	metadata.objects[object.ObjectID] = object
	if _, err := service.Fetch(context.Background(), ObjectRequest{
		ObjectID: object.ObjectID, RelayVersion: Epoch, BearerToken: "token",
	}); !errors.Is(err, ErrForbidden) {
		t.Fatalf("unbound generations: %v", err)
	}
	if len(payloads.opened) != 0 {
		t.Fatal("expired or unbound fetch released bytes")
	}
}

func TestPrepareUploadVerifiesHostProofAndExactLength(t *testing.T) {
	service, authority, metadata, _ := newTestService(t, true)
	metadata.grant = testGrant()
	object := testObject()
	metadata.objects[object.ObjectID] = object
	request := UploadRequest{
		ObjectRequest: ObjectRequest{
			ObjectID: object.ObjectID, RelayVersion: Epoch,
			SignedAtUnixMs: testNow.UnixMilli(), Signature: []byte("signed"),
		},
		ContentLength: 4, HasContentLength: true,
	}
	target, err := service.PrepareUpload(context.Background(), request)
	if err != nil {
		t.Fatal(err)
	}
	if target.Length != 4 || target.Ref.ObjectID != object.ObjectID || target.Ref.Purpose != object.Purpose {
		t.Fatalf("target = %+v", target)
	}
	// The upload signature binds the stored length and digest.
	if len(authority.fields) != 1 {
		t.Fatal("host proof missing")
	}
	if !bytes.Equal(authority.fields[0].SHA256, object.Digest) || authority.fields[0].CiphertextBytes != 4 {
		t.Fatalf("upload proof fields = %+v", authority.fields[0])
	}

	request.HasContentLength = false
	request.ContentLength = -1
	if _, err := service.PrepareUpload(context.Background(), request); !errors.Is(err, ErrInvalid) {
		t.Fatalf("missing content length: %v", err)
	}
	request.HasContentLength = true
	request.ContentLength = 5
	if _, err := service.PrepareUpload(context.Background(), request); !errors.Is(err, ErrConflict) {
		t.Fatalf("lying content length: %v", err)
	}
	request.ContentLength = 4
	authority.proofErr = auth.ErrUnauthorized
	if _, err := service.PrepareUpload(context.Background(), request); !errors.Is(err, ErrUnauthorized) {
		t.Fatalf("bad proof: %v", err)
	}
}

func TestUploadStreamsThroughPayload(t *testing.T) {
	service, _, metadata, payloads := newTestService(t, true)
	metadata.grant = testGrant()
	object := testObject()
	metadata.objects[object.ObjectID] = object
	payloads.object = object
	target, err := service.PrepareUpload(context.Background(), UploadRequest{
		ObjectRequest: ObjectRequest{
			ObjectID: object.ObjectID, RelayVersion: Epoch,
			SignedAtUnixMs: testNow.UnixMilli(), Signature: []byte("signed"),
		},
		ContentLength: 4, HasContentLength: true,
	})
	if err != nil {
		t.Fatal(err)
	}
	if err := service.Upload(context.Background(), target, bytes.NewReader([]byte("body"))); err != nil {
		t.Fatal(err)
	}
	if string(payloads.written) != "body" {
		t.Fatalf("written = %q", payloads.written)
	}
	payloads.writeErr = payload.ErrLengthMismatch
	if err := service.Upload(context.Background(), target, bytes.NewReader([]byte("body"))); !errors.Is(err, ErrConflict) {
		t.Fatalf("length translation: %v", err)
	}
	if err := service.Upload(context.Background(), UploadTarget{}, strings.NewReader("x")); !errors.Is(err, ErrInvalid) {
		t.Fatalf("incomplete target: %v", err)
	}
}

func TestFinishChecksExactLengthAndDigest(t *testing.T) {
	service, _, metadata, payloads := newTestService(t, true)
	metadata.grant = testGrant()
	object := testObject()
	metadata.objects[object.ObjectID] = object
	payloads.object = object
	base := FinishRequest{
		ObjectRequest: ObjectRequest{
			ObjectID: object.ObjectID, RelayVersion: Epoch,
			SignedAtUnixMs: testNow.UnixMilli(), Signature: []byte("signed"),
		},
		CiphertextBytes: 4, SHA256: object.Digest,
	}
	finished, err := service.Finish(context.Background(), base)
	if err != nil {
		t.Fatal(err)
	}
	if finished.ObjectID != object.ObjectID || finished.CiphertextBytes != 4 ||
		finished.Purpose != whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_RESULT ||
		finished.OwnerNodeID != "owner-1" {
		t.Fatalf("finished = %+v", finished)
	}

	base.CiphertextBytes = 5
	if _, err := service.Finish(context.Background(), base); !errors.Is(err, ErrConflict) {
		t.Fatalf("length: %v", err)
	}
	base.CiphertextBytes = 4
	base.SHA256 = digestOf([]byte("other"))
	if _, err := service.Finish(context.Background(), base); !errors.Is(err, ErrDigestMismatch) {
		t.Fatalf("digest: %v", err)
	}
	base.SHA256 = object.Digest
	payloads.finishErr = payload.ErrNotStaged
	if _, err := service.Finish(context.Background(), base); !errors.Is(err, ErrConflict) {
		t.Fatalf("not staged: %v", err)
	}
}

func TestDeleteIsScopedAndIdempotent(t *testing.T) {
	service, authority, metadata, payloads := newTestService(t, true)
	metadata.grant = testGrant()
	object := testObject()
	metadata.objects[object.ObjectID] = object
	payloads.object = object
	authority.identity = auth.Identity{DeviceID: "device-1", SessionID: "session-1"}
	request := ObjectRequest{ObjectID: object.ObjectID, RelayVersion: Epoch, BearerToken: "device-token"}

	result, err := service.Delete(context.Background(), request)
	if err != nil {
		t.Fatal(err)
	}
	if !result.Cancelled || payloads.cancelled != 1 {
		t.Fatalf("delete = %+v, cancelled %d", result, payloads.cancelled)
	}

	// A terminal object reports cancelled=false and repeats without error.
	object.State = store.PayloadDeleted
	metadata.objects[object.ObjectID] = object
	result, err = service.Delete(context.Background(), request)
	if err != nil {
		t.Fatal(err)
	}
	if result.Cancelled || payloads.cancelled != 2 {
		t.Fatalf("terminal delete = %+v", result)
	}

	// A concurrent cleanup that removed the row is an idempotent no-op.
	metadata.grant = testGrant()
	metadata.objects[object.ObjectID] = testObject()
	payloads.cancelErr = payload.ErrNotFound
	result, err = service.Delete(context.Background(), request)
	if err != nil || result.Cancelled {
		t.Fatalf("raced delete = %+v %v", result, err)
	}

	// Revoked grants may still clean up, but a foreign device may not.
	payloads.cancelErr = nil
	object = testObject()
	metadata.objects[object.ObjectID] = object
	metadata.grant = testGrant()
	metadata.grant.Active = false
	if _, err := service.Delete(context.Background(), request); err != nil {
		t.Fatalf("revoked delete: %v", err)
	}
	authority.identity = auth.Identity{DeviceID: "device-2", SessionID: "session-1"}
	if _, err := service.Delete(context.Background(), request); !errors.Is(err, ErrForbidden) {
		t.Fatalf("foreign delete: %v", err)
	}
}

func TestDeleteHostAuthorityAndWrongOwner(t *testing.T) {
	service, authority, metadata, payloads := newTestService(t, true)
	metadata.grant = testGrant()
	object := testObject()
	metadata.objects[object.ObjectID] = object
	payloads.object = object
	request := ObjectRequest{
		ObjectID: object.ObjectID, RelayVersion: Epoch,
		SignedAtUnixMs: testNow.UnixMilli(), Signature: []byte("signed"),
	}
	if _, err := service.Delete(context.Background(), request); err != nil {
		t.Fatal(err)
	}
	if len(authority.purposes) != 1 || authority.purposes[0] != auth.RelayAuthObject {
		t.Fatalf("host delete purposes = %v", authority.purposes)
	}
	request.OwnerNode = "node-2"
	_, err := service.Delete(context.Background(), request)
	if !errors.Is(err, ErrWrongOwner) {
		t.Fatalf("wrong owner: %v", err)
	}
	if payloads.cancelled != 1 {
		t.Fatal("wrong owner reached storage")
	}
}

func TestObjectHostProofBindsOperationPurpose(t *testing.T) {
	service, authority, metadata, _ := newTestService(t, true)
	metadata.grant = testGrant()
	object := testObject()
	metadata.objects[object.ObjectID] = object
	authority.proofErr = auth.ErrUnauthorized
	for name, call := range map[string]func() error{
		"fetch": func() error {
			_, err := service.Fetch(context.Background(), ObjectRequest{
				ObjectID: object.ObjectID, RelayVersion: Epoch,
				SignedAtUnixMs: testNow.UnixMilli(), Signature: []byte("signed"),
			})
			return err
		},
		"delete": func() error {
			_, err := service.Delete(context.Background(), ObjectRequest{
				ObjectID: object.ObjectID, RelayVersion: Epoch,
				SignedAtUnixMs: testNow.UnixMilli(), Signature: []byte("signed"),
			})
			return err
		},
		"finish": func() error {
			_, err := service.Finish(context.Background(), FinishRequest{
				ObjectRequest: ObjectRequest{
					ObjectID: object.ObjectID, RelayVersion: Epoch,
					SignedAtUnixMs: testNow.UnixMilli(), Signature: []byte("signed"),
				},
				CiphertextBytes: 4, SHA256: object.Digest,
			})
			return err
		},
	} {
		if err := call(); !errors.Is(err, ErrUnauthorized) {
			t.Fatalf("%s: %v", name, err)
		}
	}
}

func TestTranslatePayloadErrors(t *testing.T) {
	cases := []struct {
		name string
		in   error
		want error
	}{
		{"nil", nil, nil},
		{"disabled", payload.ErrDisabled, ErrDisabled},
		{"invalid", payload.ErrInvalid, ErrInvalid},
		{"too large", payload.ErrTooLarge, ErrTooLarge},
		{"host quota", payload.ErrQuota, ErrQuota},
		{"capacity", payload.ErrCapacity, ErrQuota},
		{"headroom", payload.ErrHeadroom, ErrPressure},
		{"duplicate", payload.ErrDuplicate, ErrConflict},
		{"not ready", payload.ErrNotReady, ErrConflict},
		{"not staged", payload.ErrNotStaged, ErrConflict},
		{"length", payload.ErrLengthMismatch, ErrConflict},
		{"store conflict", store.ErrConflict, ErrConflict},
		{"not found", payload.ErrNotFound, ErrNotFound},
		{"store not found", store.ErrNotFound, ErrNotFound},
		{"expired", payload.ErrExpired, ErrExpired},
		{"forbidden", payload.ErrForbidden, ErrForbidden},
		{"scope", store.ErrPayloadScopeMismatch, ErrForbidden},
		{"digest", payload.ErrDigestMismatch, ErrDigestMismatch},
		{"short upload", io.ErrUnexpectedEOF, ErrConflict},
		{"upload timeout", context.DeadlineExceeded, ErrUnavailable},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			got := translate(test.in)
			if test.want == nil {
				if got != nil {
					t.Fatalf("got %v", got)
				}
				return
			}
			if !errors.Is(got, test.want) {
				t.Fatalf("got %v, want %v", got, test.want)
			}
		})
	}
	raw := errors.New("database down")
	if got := translate(raw); !errors.Is(got, raw) {
		t.Fatal("raw errors must pass through as internal errors")
	}
}

func TestTranslateAuthErrors(t *testing.T) {
	cases := []struct {
		name string
		in   error
		want error
	}{
		{"unauthorized", auth.ErrUnauthorized, ErrUnauthorized},
		{"signature window", auth.ErrSignatureExpired, ErrUnauthorized},
		{"bad request", auth.ErrBadRequest, ErrInvalid},
		{"forbidden", auth.ErrForbidden, ErrForbidden},
		{"unavailable", auth.ErrUnavailable, ErrUnavailable},
		{"denial 403", &auth.AuthorityDenial{StatusCode: 403}, ErrForbidden},
		{"denial 503", &auth.AuthorityDenial{StatusCode: 503}, ErrUnavailable},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			if got := translateAuth(test.in); !errors.Is(got, test.want) {
				t.Fatalf("got %v, want %v", got, test.want)
			}
		})
	}
	raw := errors.New("database down")
	if got := translateAuth(raw); !errors.Is(got, raw) {
		t.Fatal("raw auth errors must pass through")
	}
}

func TestPurposeHelpers(t *testing.T) {
	if purposeName(whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_EVENT) != "PAYLOAD_PURPOSE_EVENT" {
		t.Fatal("purpose name")
	}
	if purposeName(0) != "" || purposeName(99) != "" {
		t.Fatal("unknown purpose name")
	}
	if purposeValue("PAYLOAD_PURPOSE_HISTORY_PAGE") != whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_HISTORY_PAGE {
		t.Fatal("purpose value")
	}
	if purposeValue("nope") != 0 {
		t.Fatal("unknown purpose value")
	}
	if int64Ptr(1<<63) != nil {
		t.Fatal("overflowing generation must not bind")
	}
	if value := int64Ptr(7); value == nil || *value != 7 {
		t.Fatal("generation pointer")
	}
}

// Interface fakes.

type fakeAuth struct {
	identity     auth.Identity
	authErr      error
	authorizeErr error
	proofErr     error
	purposes     []string
	fields       []auth.RelayFields
}

func (f *fakeAuth) AuthenticateDeviceToken(context.Context, string) (auth.Identity, error) {
	return f.identity, f.authErr
}

func (f *fakeAuth) AuthorizeHost(context.Context, auth.Identity, string) error {
	return f.authorizeErr
}

func (f *fakeAuth) AuthenticateRelayRequest(_ context.Context, purpose string, fields auth.RelayFields, signature []byte) error {
	f.purposes = append(f.purposes, purpose)
	f.fields = append(f.fields, fields)
	if f.proofErr != nil {
		return f.proofErr
	}
	if len(signature) == 0 {
		return auth.ErrUnauthorized
	}
	return nil
}

type fakeStore struct {
	objects  map[string]store.PayloadObject
	grant    store.AdmissionGrant
	grantErr error
}

func (f *fakeStore) GetPayload(_ context.Context, objectID string) (store.PayloadObject, error) {
	object, ok := f.objects[objectID]
	if !ok {
		return store.PayloadObject{}, store.ErrNotFound
	}
	return object, nil
}

func (f *fakeStore) AdmissionGrantFor(context.Context, string, string) (store.AdmissionGrant, error) {
	return f.grant, f.grantErr
}

type fakePayloads struct {
	reserved   []payload.ReserveInput
	reserveErr error
	written    []byte
	writeErr   error
	object     payload.Object
	finishErr  error
	opened     []payload.Ref
	openErr    error
	cancelled  int
	cancelErr  error
}

func (f *fakePayloads) Reserve(_ context.Context, input payload.ReserveInput) (payload.Object, error) {
	if f.reserveErr != nil {
		return payload.Object{}, f.reserveErr
	}
	f.reserved = append(f.reserved, input)
	f.object = payload.Object{
		ObjectID: input.ObjectID, NodeID: "node-1", OwnerID: "owner-1",
		HostID: input.HostID, DeviceID: input.DeviceID, AppSession: input.AppSession,
		ChannelID: input.ChannelID, Purpose: input.Purpose, PartID: input.PartID,
		State: store.PayloadReserved, Length: input.Length, Digest: input.Digest,
		ExpiresAt:       input.ParentDeadline,
		GrantGeneration: input.GrantGeneration, RevocationGeneration: input.RevocationGeneration,
		DeviceRevocationGeneration: input.DeviceRevocationGeneration,
	}
	return f.object, nil
}

func (f *fakePayloads) Write(_ context.Context, _ payload.Ref, body io.Reader) error {
	if f.writeErr != nil {
		return f.writeErr
	}
	data, err := io.ReadAll(body)
	if err != nil {
		return err
	}
	f.written = append(f.written, data...)
	return nil
}

func (f *fakePayloads) Finish(context.Context, payload.Ref) (payload.Object, error) {
	return f.object, f.finishErr
}

func (f *fakePayloads) Open(_ context.Context, ref payload.Ref) (io.ReadCloser, payload.Object, error) {
	if f.openErr != nil {
		return nil, payload.Object{}, f.openErr
	}
	f.opened = append(f.opened, ref)
	return io.NopCloser(bytes.NewReader([]byte("body"))), f.object, nil
}

func (f *fakePayloads) Cancel(context.Context, payload.Ref) error {
	f.cancelled++
	return f.cancelErr
}
