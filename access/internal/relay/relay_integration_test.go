//go:build integration && unix

package relay_test

import (
	"bytes"
	"context"
	"crypto/sha256"
	"errors"
	"io"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/auth"
	"github.com/whalerein/whalerein/access/internal/payload"
	"github.com/whalerein/whalerein/access/internal/relay"
	"github.com/whalerein/whalerein/access/internal/store/storetest"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// integrationAuth accepts the single device identity the test seeds. The relay
// proof path is covered by auth unit vectors; this test proves the store and
// payload seams line up for reservation, release and revocation.
type integrationAuth struct{}

func (integrationAuth) AuthenticateDeviceToken(context.Context, string) (auth.Identity, error) {
	return auth.Identity{DeviceID: "device-1"}, nil
}

func (integrationAuth) AuthorizeHost(context.Context, auth.Identity, string) error { return nil }

func (integrationAuth) AuthenticateRelayRequest(context.Context, string, auth.RelayFields, []byte) error {
	return nil
}

func integrationPayloadConfig(root string) payload.Config {
	return payload.Config{
		Enabled: true, RootDir: root, NodeID: "node-1",
		MaxObjectBytes: 1 << 20, MaxHostBytes: 4 << 20, MaxNodeBytes: 8 << 20,
		MaxObjects: 10, MaxActiveUploads: 4, FreeSpaceReserveBytes: 1,
		Retention: time.Hour, CleanupInterval: time.Minute, CleanupBatch: 10,
		UploadTimeout: 5 * time.Second, ReadTimeout: 5 * time.Second,
	}
}

func TestIntegrationReserveFetchAndRevokeFence(t *testing.T) {
	ctx := context.Background()
	db, _ := storetest.New(t)
	if _, err := db.EnsureHost(ctx, "host-1"); err != nil {
		t.Fatal(err)
	}
	if err := db.CreatePairedDevice(ctx, "host-1", "device-1", "fp", []byte("auth-device-1"), time.Now().UTC()); err != nil {
		t.Fatal(err)
	}
	storage, err := payload.New(payload.Dependencies{Store: db, Config: integrationPayloadConfig(t.TempDir())})
	if err != nil {
		t.Fatal(err)
	}
	service, err := relay.New(relay.Dependencies{
		Auth: integrationAuth{}, Store: db, Payloads: storage,
		Config: relay.Config{
			Enabled: true, NodeID: "node-1", Retention: time.Hour,
			UploadTimeout: 5 * time.Second, ReadTimeout: 5 * time.Second,
		},
	})
	if err != nil {
		t.Fatal(err)
	}

	body := []byte("ciphertext-body")
	digest := sha256.Sum256(body)
	reservation, err := service.Reserve(ctx, relay.ReserveRequest{
		RelayVersion: relay.Epoch, HostID: "host-1", DeviceID: "device-1",
		ChannelID:       "channel-1",
		Purpose:         whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_RESULT,
		CiphertextBytes: uint64(len(body)), SHA256: digest[:],
		ExpiresAtUnixMs: time.Now().Add(5 * time.Minute).UnixMilli(),
		BearerToken:     "device-token",
	})
	if err != nil {
		t.Fatal(err)
	}
	stored, err := db.GetPayload(ctx, reservation.ObjectID)
	if err != nil {
		t.Fatal(err)
	}
	grant, err := db.AdmissionGrantFor(ctx, "host-1", "device-1")
	if err != nil {
		t.Fatal(err)
	}
	if stored.GrantGeneration == nil || *stored.GrantGeneration != int64(grant.GrantGeneration) ||
		stored.RevocationGeneration == nil || *stored.RevocationGeneration != int64(grant.RevocationGeneration) ||
		stored.DeviceRevocationGeneration == nil || *stored.DeviceRevocationGeneration != int64(grant.DeviceRevocationGeneration) {
		t.Fatalf("stored revisions do not match the active grant: %+v vs %+v", stored, grant)
	}

	request := relay.ObjectRequest{ObjectID: reservation.ObjectID, RelayVersion: relay.Epoch, BearerToken: "device-token"}
	target, err := service.PrepareUpload(ctx, relay.UploadRequest{
		ObjectRequest: request, ContentLength: int64(len(body)), HasContentLength: true,
	})
	if err != nil {
		t.Fatal(err)
	}
	if err := service.Upload(ctx, target, bytes.NewReader(body)); err != nil {
		t.Fatal(err)
	}
	object, err := service.Finish(ctx, relay.FinishRequest{
		ObjectRequest: request, CiphertextBytes: uint64(len(body)), SHA256: digest[:],
	})
	if err != nil {
		t.Fatal(err)
	}
	if object.ObjectID != reservation.ObjectID || !bytes.Equal(object.SHA256, digest[:]) {
		t.Fatalf("finished object = %+v", object)
	}

	fetched, err := service.Fetch(ctx, request)
	if err != nil {
		t.Fatal(err)
	}
	data, err := io.ReadAll(fetched.Body)
	_ = fetched.Body.Close()
	if err != nil || !bytes.Equal(data, body) {
		t.Fatalf("fetched body mismatch: %v", err)
	}

	// A revoke fences the old reference without releasing bytes.
	if err := db.RevokeGrant(ctx, "host-1", "device-1", time.Now().UTC()); err != nil {
		t.Fatal(err)
	}
	if _, err := service.Fetch(ctx, request); !errors.Is(err, relay.ErrForbidden) {
		t.Fatalf("revoked fetch = %v", err)
	}
	// Cancellation remains available for the same authorized scope and is
	// idempotent.
	deleted, err := service.Delete(ctx, request)
	if err != nil || !deleted.Cancelled {
		t.Fatalf("delete after revoke = %+v %v", deleted, err)
	}
	deleted, err = service.Delete(ctx, request)
	if err != nil || deleted.Cancelled {
		t.Fatalf("repeated delete = %+v %v", deleted, err)
	}
}
