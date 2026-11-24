package relay

import (
	"context"
	"errors"
	"strings"
	"testing"

	"github.com/whalerein/whalerein/access/internal/auth"
)

// ACC-6 regression: authentication precedes object lookup.
//
// The relay package contract authorizes the caller before releasing any object
// state, and a missing object must never answer differently from a rejected
// proof for an unauthenticated caller. A Device bearer is verified before the
// store read; a Host proof needs the object owner first, so a missing object on
// that path is reported as unauthorized. An unauthenticated caller must not be
// able to distinguish a real 32-hex object id from a missing one: both answer
// ErrUnauthorized (401).
func TestReproRelayDoesNotRevealObjectExistenceBeforeAuthentication(t *testing.T) {
	service, authority, metadata, _ := newTestService(t, true)
	metadata.grant = testGrant()
	metadata.objects[testObject().ObjectID] = testObject()
	missing := strings.Repeat("f", 32)
	if _, exists := metadata.objects[missing]; exists {
		t.Fatal("fixture error: missing id is present")
	}

	// Device-bearer path with an invalid credential.
	authority.authErr = auth.ErrUnauthorized
	for _, tc := range []struct {
		name   string
		object string
	}{
		{"existing object", testObject().ObjectID},
		{"missing object", missing},
	} {
		t.Run("device token/"+tc.name, func(t *testing.T) {
			_, err := service.Fetch(context.Background(), ObjectRequest{
				ObjectID: tc.object, RelayVersion: Epoch, BearerToken: "not-a-valid-token",
			})
			if !errors.Is(err, ErrUnauthorized) {
				t.Fatalf("bad device token on %s: err = %v, want ErrUnauthorized (401); "+
					"an unauthenticated caller must not learn whether the object exists",
					tc.name, err)
			}
		})
	}

	// Host-signature path with an invalid signature.
	authority.authErr = nil
	authority.proofErr = auth.ErrUnauthorized
	for _, tc := range []struct {
		name   string
		object string
	}{
		{"existing object", testObject().ObjectID},
		{"missing object", missing},
	} {
		t.Run("host signature/"+tc.name, func(t *testing.T) {
			_, err := service.Fetch(context.Background(), ObjectRequest{
				ObjectID: tc.object, RelayVersion: Epoch,
				SignedAtUnixMs: testNow.UnixMilli(), Signature: []byte("bogus-signature"),
			})
			if !errors.Is(err, ErrUnauthorized) {
				t.Fatalf("bad host signature on %s: err = %v, want ErrUnauthorized (401); "+
					"an unauthenticated caller must not learn whether the object exists",
					tc.name, err)
			}
		})
	}

	// Positive control: a fully authorized caller still gets the documented
	// 404 for an unknown object.
	authority.proofErr = nil
	authority.identity = auth.Identity{DeviceID: "device-1", SessionID: "session-1"}
	for _, tc := range []struct {
		name   string
		object string
		want   error
	}{
		{"authorized existing object", testObject().ObjectID, nil},
		{"authorized missing object", missing, ErrNotFound},
	} {
		t.Run(tc.name, func(t *testing.T) {
			_, err := service.Fetch(context.Background(), ObjectRequest{
				ObjectID: tc.object, RelayVersion: Epoch, BearerToken: "device-token",
			})
			if !errors.Is(err, tc.want) {
				t.Fatalf("authorized fetch: err = %v, want %v", err, tc.want)
			}
		})
	}
}
