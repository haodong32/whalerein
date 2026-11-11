package pairing

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/url"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

type fakeStore struct {
	hosts   map[string]store.Host
	tokens  map[string]store.PairingToken
	devices map[string]struct {
		fingerprint string
		authHash    []byte
	}
	grants    map[string]bool
	failures  map[string]int
	used      map[string]time.Time
	meta      []string
	createErr error
	lookupErr error
	incErr    error
	useErr    error
	useResult *bool
	deviceErr error
	metaErr   error
}

func newFakeStore() *fakeStore {
	return &fakeStore{
		hosts:  map[string]store.Host{},
		tokens: map[string]store.PairingToken{},
		devices: map[string]struct {
			fingerprint string
			authHash    []byte
		}{},
		grants:   map[string]bool{},
		failures: map[string]int{},
		used:     map[string]time.Time{},
	}
}

func hashKey(hash []byte) string { return string(hash) }

func (f *fakeStore) GetHost(_ context.Context, hostID string) (store.Host, error) {
	host, ok := f.hosts[hostID]
	if !ok {
		return store.Host{}, store.ErrNotFound
	}
	return host, nil
}

func (f *fakeStore) CreatePairingToken(_ context.Context, tokenHash []byte, hostID string, expiresAt, at time.Time) error {
	if f.createErr != nil {
		return f.createErr
	}
	if _, ok := f.tokens[hashKey(tokenHash)]; ok {
		return store.ErrConflict
	}
	f.tokens[hashKey(tokenHash)] = store.PairingToken{TokenHash: tokenHash, HostID: hostID, ExpiresAt: expiresAt, CreatedAt: at, UpdatedAt: at}
	return nil
}

func (f *fakeStore) PairingTokenByHash(_ context.Context, tokenHash []byte) (store.PairingToken, error) {
	if f.lookupErr != nil {
		return store.PairingToken{}, f.lookupErr
	}
	token, ok := f.tokens[hashKey(tokenHash)]
	if !ok {
		return store.PairingToken{}, store.ErrNotFound
	}
	token.FailureCount = f.failures[hashKey(tokenHash)]
	if used, ok := f.used[hashKey(tokenHash)]; ok {
		token.UsedAt = &used
	}
	return token, nil
}

func (f *fakeStore) IncrementPairingFailure(_ context.Context, tokenHash []byte, _ time.Time) (int, error) {
	if f.incErr != nil {
		return 0, f.incErr
	}
	key := hashKey(tokenHash)
	f.failures[key]++
	return f.failures[key], nil
}

func (f *fakeStore) MarkPairingTokenUsed(_ context.Context, tokenHash []byte, at time.Time) (bool, error) {
	if f.useErr != nil {
		return false, f.useErr
	}
	if f.useResult != nil {
		return *f.useResult, nil
	}
	key := hashKey(tokenHash)
	if _, ok := f.used[key]; ok {
		return false, nil
	}
	f.used[key] = at
	return true, nil
}

func (f *fakeStore) CreatePairedDevice(_ context.Context, hostID, deviceID, fingerprint string, authHash []byte, _ time.Time) error {
	if f.deviceErr != nil {
		return f.deviceErr
	}
	f.devices[deviceID] = struct {
		fingerprint string
		authHash    []byte
	}{fingerprint: fingerprint, authHash: append([]byte(nil), authHash...)}
	f.grants[hostID+"|"+deviceID] = true
	return nil
}

func (f *fakeStore) CommitPairing(ctx context.Context, tokenHash []byte, hostID, deviceID, fingerprint string, authHash []byte, at time.Time, _ *whalereinv1.HostRequest) (bool, error) {
	if f.deviceErr != nil {
		return false, f.deviceErr
	}
	if existing, ok := f.devices[deviceID]; ok && existing.fingerprint != fingerprint {
		return false, store.ErrConflict
	}
	used, err := f.MarkPairingTokenUsed(ctx, tokenHash, at)
	if err != nil || !used {
		return used, err
	}
	return true, f.CreatePairedDevice(ctx, hostID, deviceID, fingerprint, authHash, at)
}

func (f *fakeStore) UpdateHostMetadata(_ context.Context, hostID, fingerprint string, protocolVersion uint32, _ time.Time) error {
	if f.metaErr != nil {
		return f.metaErr
	}
	f.meta = append(f.meta, hostID+":"+fingerprint+":"+string(rune('0'+protocolVersion)))
	return nil
}

type fakeTransport struct {
	requests []*whalereinv1.HostRequest
	awaitFn  func(*whalereinv1.HostRequest) (*whalereinv1.HostResult, error)
	err      error
}

func (f *fakeTransport) PublishAndAwait(_ context.Context, _ string, req *whalereinv1.HostRequest, _ time.Duration) (*whalereinv1.HostResult, error) {
	f.requests = append(f.requests, req)
	if f.err != nil {
		return nil, f.err
	}
	if f.awaitFn != nil {
		return f.awaitFn(req)
	}
	return nil, errors.New("no await function")
}

type fakePresence struct{ online map[string]bool }

func (f *fakePresence) IsHostOnline(_ context.Context, hostID string) bool { return f.online[hostID] }

func (f *fakeTransport) SupportsPairing(string) bool { return true }

type fixture struct {
	service   *Service
	store     *fakeStore
	transport *fakeTransport
	presence  *fakePresence
	now       time.Time
}

func newFixture(t *testing.T) *fixture {
	t.Helper()
	fs := newFakeStore()
	fs.hosts["h1"] = store.Host{HostID: "h1"}
	transport := &fakeTransport{}
	transport.awaitFn = defaultPairingAnswers
	presence := &fakePresence{online: map[string]bool{"h1": true}}
	now := time.Date(2026, 11, 9, 12, 0, 0, 0, time.UTC)
	fixtureRef := &fixture{now: now}
	service, err := New(Dependencies{
		Store: fs, Transport: transport, Presence: presence,
		Config: Config{
			PublicURL: "https://access.example.com/", MQTTURL: "mqtts://access.example.com:443",
			InvitationTTL: 10 * time.Minute, ClaimTimeout: time.Second, ClaimTTL: time.Minute,
			MaxFailures: 3, ProtocolVersion: 1,
		},
		Metrics: diagnostics.NewMetrics(),
		Masker:  diagnostics.NewMaskerWithKey([]byte("pair")),
		Logger:  slog.New(slog.NewTextHandler(io.Discard, nil)),
		Now:     func() time.Time { return fixtureRef.now },
		Random:  bytes.NewReader(bytes.Repeat([]byte{0x42}, 4096)),
	})
	if err != nil {
		t.Fatal(err)
	}
	fixtureRef.service = service
	fixtureRef.store = fs
	fixtureRef.transport = transport
	fixtureRef.presence = presence
	return fixtureRef
}

func defaultPairingAnswers(req *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
	var claim whalereinv1.PairingClaim
	if err := proto.Unmarshal(req.GetBody(), &claim); err != nil {
		return nil, err
	}
	var result whalereinv1.PairingClaimResult
	switch claim.GetPhase() {
	case whalereinv1.PairingPhase_PAIRING_PHASE_HELLO:
		result = whalereinv1.PairingClaimResult{NoiseMessage: []byte("msg2"), HostPublicKey: []byte("host-key"), HostFingerprint: "host-fp"}
	case whalereinv1.PairingPhase_PAIRING_PHASE_FINISH:
		result = whalereinv1.PairingClaimResult{DeviceId: claim.GetDeviceId(), DevicePublicKey: bytes.Repeat([]byte{4}, 32)}
	case whalereinv1.PairingPhase_PAIRING_PHASE_AUTHORIZE, whalereinv1.PairingPhase_PAIRING_PHASE_COMMIT:
		result = whalereinv1.PairingClaimResult{Authorized: true, DeviceId: claim.GetDeviceId()}
	}
	body, _ := proto.Marshal(&result)
	return &whalereinv1.HostResult{
		RequestId: req.GetRequestId(), HostId: req.GetHostId(), DeviceId: claim.GetDeviceId(),
		Status: whalereinv1.OnlineStatus_succeeded, ResultBody: body,
	}, nil
}

func (f *fixture) invite(t *testing.T) (*whalereinv1.PairingInvitation, string) {
	t.Helper()
	invitation, err := f.service.CreateInvitation(context.Background(), "h1")
	if err != nil {
		t.Fatal(err)
	}
	hash := sha256.Sum256([]byte(invitation.GetToken()))
	f.store.tokens[hashKey(hash[:])] = store.PairingToken{TokenHash: hash[:], HostID: "h1", ExpiresAt: f.now.Add(10 * time.Minute)}
	return invitation, invitation.GetToken()
}

func helloRequest(token, claimID, deviceID string) *whalereinv1.PairingClaimRequest {
	return &whalereinv1.PairingClaimRequest{
		Token: token, ClaimId: claimID, Phase: whalereinv1.PairingPhase_PAIRING_PHASE_HELLO,
		NoiseMessage: []byte("msg1"), DeviceId: deviceID, ProtocolVersion: 1,
	}
}

func finishRequest(token, claimID, deviceID string) *whalereinv1.PairingClaimRequest {
	return &whalereinv1.PairingClaimRequest{
		Token: token, ClaimId: claimID, Phase: whalereinv1.PairingPhase_PAIRING_PHASE_FINISH,
		NoiseMessage: []byte("msg3"), DeviceId: deviceID, ProtocolVersion: 1, Confirmed: true,
	}
}

func TestNewRejectsMissingRequiredDependencies(t *testing.T) {
	if _, err := New(Dependencies{}); err == nil {
		t.Fatal("expected error")
	}
	if _, err := New(Dependencies{Store: newFakeStore()}); err == nil {
		t.Fatal("expected transport error")
	}
	if _, err := New(Dependencies{Store: newFakeStore(), Transport: &fakeTransport{}}); err == nil {
		t.Fatal("expected presence error")
	}
}

func TestCreateInvitationStoresOnlyHashAndReturnsBoundToken(t *testing.T) {
	f := newFixture(t)
	invitation, err := f.service.CreateInvitation(context.Background(), "h1")
	if err != nil {
		t.Fatal(err)
	}
	if len(invitation.GetToken()) != 43 || invitation.GetHostId() != "h1" || invitation.GetExpiresAtUnixMs() != f.now.Add(10*time.Minute).UnixMilli() {
		t.Fatalf("invitation = %+v", invitation)
	}
	hash := sha256.Sum256([]byte(invitation.GetToken()))
	if _, ok := f.store.tokens[hashKey(hash[:])]; !ok {
		t.Fatal("token hash not stored")
	}
	uri, err := url.Parse(invitation.GetPairingUri())
	if err != nil {
		t.Fatal(err)
	}
	if uri.Scheme != "whalerein" || uri.Host != "pair" {
		t.Fatalf("uri = %s", uri)
	}
	query := uri.Query()
	if query.Get("v") != "1" || query.Get("access") != "https://access.example.com" || query.Get("host") != "h1" || query.Get("token") != invitation.GetToken() || query.Get("mqtt") != "mqtts://access.example.com:443" {
		t.Fatalf("query = %v", query)
	}
	if _, err := f.service.CreateInvitation(context.Background(), "missing"); !errors.Is(err, ErrHostNotFound) {
		t.Fatalf("missing host = %v", err)
	}
	if _, err := f.service.CreateInvitation(context.Background(), " "); !errors.Is(err, ErrInvalidClaim) {
		t.Fatalf("empty host = %v", err)
	}
	f.store.createErr = errors.New("db down")
	if _, err := f.service.CreateInvitation(context.Background(), "h1"); err == nil {
		t.Fatal("expected create error")
	}
	f.store.createErr = nil
	f.service.random = bytes.NewReader(nil)
	if _, err := f.service.CreateInvitation(context.Background(), "h1"); err == nil {
		t.Fatal("expected random error")
	}
}

func TestClaimRejectsInvalidExpiredUsedInvitationsAndUnconfirmedFinish(t *testing.T) {
	f := newFixture(t)
	ctx := context.Background()
	if _, err := f.service.Claim(ctx, nil); !errors.Is(err, ErrInvalidClaim) {
		t.Fatalf("nil = %v", err)
	}
	base := helloRequest("t", "c", "d")
	for name, mutate := range map[string]func(*whalereinv1.PairingClaimRequest){
		"rejects claim without invitation token":               func(r *whalereinv1.PairingClaimRequest) { r.Token = "" },
		"rejects claim without claim ID":                       func(r *whalereinv1.PairingClaimRequest) { r.ClaimId = "" },
		"rejects claim without device ID":                      func(r *whalereinv1.PairingClaimRequest) { r.DeviceId = "" },
		"rejects pairing claim without Noise message":          func(r *whalereinv1.PairingClaimRequest) { r.NoiseMessage = nil },
		"rejects unspecified pairing phase":                    func(r *whalereinv1.PairingClaimRequest) { r.Phase = whalereinv1.PairingPhase_PAIRING_PHASE_UNSPECIFIED },
		"rejects client claim using host-only authorize phase": func(r *whalereinv1.PairingClaimRequest) { r.Phase = whalereinv1.PairingPhase_PAIRING_PHASE_AUTHORIZE },
		"rejects unsupported pairing protocol version":         func(r *whalereinv1.PairingClaimRequest) { r.ProtocolVersion = 2 },
	} {
		t.Run(name, func(t *testing.T) {
			request := proto.Clone(base).(*whalereinv1.PairingClaimRequest)
			mutate(request)
			if _, err := f.service.Claim(ctx, request); !errors.Is(err, ErrInvalidClaim) && !errors.Is(err, ErrProtocolVersion) {
				t.Fatalf("err = %v", err)
			}
		})
	}
	// FINISH requires explicit fingerprint confirmation.
	finish := finishRequest("t", "c", "d")
	finish.Confirmed = false
	if _, err := f.service.Claim(ctx, finish); !errors.Is(err, ErrInvalidClaim) {
		t.Fatalf("unconfirmed = %v", err)
	}
	if _, err := f.service.Claim(ctx, helloRequest("missing", "c", "d")); !errors.Is(err, ErrUnauthorized) {
		t.Fatalf("unknown token = %v", err)
	}

	_, token := f.invite(t)
	hash := sha256.Sum256([]byte(token))
	f.store.used[hashKey(hash[:])] = f.now
	if _, err := f.service.Claim(ctx, helloRequest(token, "c", "d")); !errors.Is(err, ErrTokenUsed) {
		t.Fatalf("used = %v", err)
	}
	delete(f.store.used, hashKey(hash[:]))
	f.store.tokens[hashKey(hash[:])] = store.PairingToken{TokenHash: hash[:], HostID: "h1", ExpiresAt: f.now.Add(-time.Minute)}
	if _, err := f.service.Claim(ctx, helloRequest(token, "c", "d")); !errors.Is(err, ErrTokenExpired) {
		t.Fatalf("expired = %v", err)
	}
	f.store.tokens[hashKey(hash[:])] = store.PairingToken{TokenHash: hash[:], HostID: "h1", ExpiresAt: f.now.Add(time.Minute)}
	f.store.failures[hashKey(hash[:])] = 3
	if _, err := f.service.Claim(ctx, helloRequest(token, "c", "d")); !errors.Is(err, ErrFailureBudget) {
		t.Fatalf("budget = %v", err)
	}
	f.store.failures[hashKey(hash[:])] = 0
	f.store.lookupErr = errors.New("db down")
	if _, err := f.service.Claim(ctx, helloRequest(token, "c", "d")); err == nil {
		t.Fatal("expected lookup error")
	}
	f.store.lookupErr = nil
}

func TestHelloClaimForwardsNoiseAndReturnsHostResponse(t *testing.T) {
	f := newFixture(t)
	ctx := context.Background()
	_, token := f.invite(t)
	response, err := f.service.Claim(ctx, helloRequest(token, "claim-1", "d1"))
	if err != nil {
		t.Fatal(err)
	}
	if response.GetPhase() != whalereinv1.PairingPhase_PAIRING_PHASE_HELLO || !bytes.Equal(response.GetNoiseMessage(), []byte("msg2")) ||
		response.GetHostFingerprint() != "host-fp" || response.GetHostId() != "h1" {
		t.Fatalf("response = %+v", response)
	}
	if len(f.store.devices) != 0 || len(f.store.grants) != 0 {
		t.Fatal("HELLO must not write devices or grants")
	}
	if len(f.transport.requests) != 1 || f.transport.requests[0].GetKind() != whalereinv1.RequestKind_REQUEST_KIND_PAIRING_CLAIM {
		t.Fatalf("requests = %+v", f.transport.requests)
	}
	var claim whalereinv1.PairingClaim
	if err := proto.Unmarshal(f.transport.requests[0].GetBody(), &claim); err != nil {
		t.Fatal(err)
	}
	if claim.GetTokenHash() != hex.EncodeToString(mustHash(token)) || claim.GetPhase() != whalereinv1.PairingPhase_PAIRING_PHASE_HELLO {
		t.Fatalf("claim = %+v", &claim)
	}
	// FINISH without a matching HELLO is rejected.
	other := finishRequest(token, "other-claim", "d1")
	if _, err := f.service.Claim(ctx, other); !errors.Is(err, ErrClaimRequired) {
		t.Fatalf("missing claim = %v", err)
	}
}

func mustHash(token string) []byte {
	hash := sha256.Sum256([]byte(token))
	return hash[:]
}

func TestHelloRejectsOfflineHostAndInvalidInvitation(t *testing.T) {
	f := newFixture(t)
	ctx := context.Background()
	_, token := f.invite(t)
	hash := sha256.Sum256([]byte(token))

	f.presence.online["h1"] = false
	if _, err := f.service.Claim(ctx, helloRequest(token, "c1", "d1")); !errors.Is(err, ErrHostOffline) {
		t.Fatalf("offline = %v", err)
	}
	if f.store.failures[hashKey(hash[:])] != 1 {
		t.Fatalf("failures = %d", f.store.failures[hashKey(hash[:])])
	}
	f.presence.online["h1"] = true
	f.transport.err = errors.New("mqtt down")
	if _, err := f.service.Claim(ctx, helloRequest(token, "c2", "d1")); !errors.Is(err, ErrHostUnavailable) {
		t.Fatalf("transport = %v", err)
	}
	if f.store.failures[hashKey(hash[:])] != 2 {
		t.Fatalf("failures = %d", f.store.failures[hashKey(hash[:])])
	}
	f.transport.err = nil

	for name, fn := range map[string]func(*whalereinv1.HostRequest) (*whalereinv1.HostResult, error){
		"rejects HELLO when host reports rejected status": func(req *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
			return &whalereinv1.HostResult{Status: whalereinv1.OnlineStatus_rejected}, nil
		},
		"rejects HELLO when host result reports failure": func(req *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
			return &whalereinv1.HostResult{Status: whalereinv1.OnlineStatus_failed}, nil
		},
		"rejects HELLO with malformed host response": func(req *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
			return &whalereinv1.HostResult{Status: whalereinv1.OnlineStatus_succeeded, ResultBody: []byte("bad")}, nil
		},
		"rejects HELLO response without Noise message": func(req *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
			body, _ := proto.Marshal(&whalereinv1.PairingClaimResult{HostFingerprint: "fp"})
			return &whalereinv1.HostResult{Status: whalereinv1.OnlineStatus_succeeded, ResultBody: body}, nil
		},
	} {
		t.Run(name, func(t *testing.T) {
			f.transport.awaitFn = fn
			if _, err := f.service.Claim(ctx, helloRequest(token, "cx", "d1")); err == nil {
				t.Fatal("expected error")
			}
		})
	}
	f.store.failures[hashKey(hash[:])] = 0
	f.store.incErr = errors.New("db down")
	f.presence.online["h1"] = false
	if _, err := f.service.Claim(ctx, helloRequest(token, "cy", "d1")); !errors.Is(err, ErrHostOffline) {
		t.Fatalf("offline with inc error = %v", err)
	}
}

func TestFinishAuthorizesDeviceBeforeReturningTransportToken(t *testing.T) {
	f := newFixture(t)
	ctx := context.Background()
	_, token := f.invite(t)
	if _, err := f.service.Claim(ctx, helloRequest(token, "claim-1", "d1")); err != nil {
		t.Fatal(err)
	}
	response, err := f.service.Claim(ctx, finishRequest(token, "claim-1", "d1"))
	if err != nil {
		t.Fatal(err)
	}
	if response.GetDeviceToken() == "" || response.GetPhase() != whalereinv1.PairingPhase_PAIRING_PHASE_FINISH ||
		!bytes.Equal(response.GetDevicePublicKey(), bytes.Repeat([]byte{4}, 32)) || response.GetDeviceId() != "d1" {
		t.Fatalf("response = %+v", response)
	}
	device, ok := f.store.devices["d1"]
	if !ok {
		t.Fatal("device not written")
	}
	if device.fingerprint != hex.EncodeToString(sha256Sum(bytes.Repeat([]byte{4}, 32))) {
		t.Fatalf("fingerprint = %q", device.fingerprint)
	}
	wantHash := sha256.Sum256([]byte(response.GetDeviceToken()))
	if !bytes.Equal(device.authHash, wantHash[:]) {
		t.Fatal("device auth hash mismatch")
	}
	if !f.store.grants["h1|d1"] {
		t.Fatal("grant not active")
	}
	if _, used := f.store.used[hashKey(mustHash(token))]; !used {
		t.Fatal("token not marked used")
	}
	if len(f.store.meta) != 1 || f.store.meta[0] != "h1:host-fp:1" {
		t.Fatalf("metadata = %v", f.store.meta)
	}
	kinds := []whalereinv1.RequestKind{}
	for _, req := range f.transport.requests {
		kinds = append(kinds, req.GetKind())
	}
	wantKinds := []whalereinv1.RequestKind{
		whalereinv1.RequestKind_REQUEST_KIND_PAIRING_CLAIM,
		whalereinv1.RequestKind_REQUEST_KIND_PAIRING_CLAIM,
		whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE,
		whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE,
	}
	if len(kinds) != len(wantKinds) {
		t.Fatalf("kinds = %v", kinds)
	}
	for i := range wantKinds {
		if kinds[i] != wantKinds[i] {
			t.Fatalf("kinds = %v", kinds)
		}
	}
	var authorize whalereinv1.PairingClaim
	if err := proto.Unmarshal(f.transport.requests[2].GetBody(), &authorize); err != nil {
		t.Fatal(err)
	}
	if authorize.GetPhase() != whalereinv1.PairingPhase_PAIRING_PHASE_AUTHORIZE || !bytes.Equal(authorize.GetDevicePublicKey(), bytes.Repeat([]byte{4}, 32)) {
		t.Fatalf("authorize = %+v", &authorize)
	}
	// A used token is rejected forever.
	if _, err := f.service.Claim(ctx, helloRequest(token, "claim-2", "d1")); !errors.Is(err, ErrTokenUsed) {
		t.Fatalf("second use = %v", err)
	}
}

func sha256Sum(value []byte) []byte {
	hash := sha256.Sum256(value)
	return hash[:]
}

func TestFinishRejectsInvalidClaimsWithoutCreatingGrant(t *testing.T) {
	ctx := context.Background()
	for name, tc := range map[string]struct {
		answer  func(*whalereinv1.HostRequest) (*whalereinv1.HostResult, error)
		wantErr error
	}{
		"rejects FINISH when device ID differs from HELLO claim": {
			answer: func(req *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
				body, _ := proto.Marshal(&whalereinv1.PairingClaimResult{DeviceId: "other", DevicePublicKey: []byte("key")})
				return &whalereinv1.HostResult{Status: whalereinv1.OnlineStatus_succeeded, ResultBody: body}, nil
			},
			wantErr: ErrHostRejected,
		},
		"rejects FINISH response without device public key": {
			answer: func(req *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
				body, _ := proto.Marshal(&whalereinv1.PairingClaimResult{DeviceId: "d1"})
				return &whalereinv1.HostResult{Status: whalereinv1.OnlineStatus_succeeded, ResultBody: body}, nil
			},
			wantErr: ErrHostRejected,
		},
		"rejects FINISH response with malformed result body": {
			answer: func(req *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
				return &whalereinv1.HostResult{Status: whalereinv1.OnlineStatus_succeeded, ResultBody: []byte("bad")}, nil
			},
			wantErr: ErrHostRejected,
		},
	} {
		t.Run(name, func(t *testing.T) {
			f := newFixture(t)
			_, token := f.invite(t)
			if _, err := f.service.Claim(ctx, helloRequest(token, "claim", "d1")); err != nil {
				t.Fatal(err)
			}
			f.transport.awaitFn = tc.answer
			if _, err := f.service.Claim(ctx, finishRequest(token, "claim", "d1")); !errors.Is(err, tc.wantErr) {
				t.Fatalf("err = %v, want %v", err, tc.wantErr)
			}
			if len(f.store.devices) != 0 {
				t.Fatal("device written on failure")
			}
		})
	}
}

func TestFinishFailuresPreserveUncommittedStateAndExistingAuthorization(t *testing.T) {
	ctx := context.Background()
	assertUncommitted := func(t *testing.T, f *fixture, token string) {
		t.Helper()
		if len(f.store.devices) != 0 || len(f.store.grants) != 0 {
			t.Fatal("failed FINISH persisted device or grant state")
		}
		if _, used := f.store.used[hashKey(mustHash(token))]; used {
			t.Fatal("failed FINISH consumed its invitation")
		}
		for _, request := range f.transport.requests {
			var claim whalereinv1.PairingClaim
			if err := proto.Unmarshal(request.Body, &claim); err != nil {
				t.Fatal("pairing transport contained an invalid claim")
			}
			if claim.Phase == whalereinv1.PairingPhase_PAIRING_PHASE_COMMIT {
				t.Fatal("failed FINISH activated a host peer")
			}
		}
	}
	base := func(t *testing.T) (*fixture, string) {
		f := newFixture(t)
		_, token := f.invite(t)
		if _, err := f.service.Claim(ctx, helloRequest(token, "claim", "d1")); err != nil {
			t.Fatal(err)
		}
		return f, token
	}

	t.Run("host authorization denial persists no grant and leaves invitation unused", func(t *testing.T) {
		f, token := base(t)
		f.transport.awaitFn = func(req *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
			var claim whalereinv1.PairingClaim
			_ = proto.Unmarshal(req.GetBody(), &claim)
			if claim.GetPhase() == whalereinv1.PairingPhase_PAIRING_PHASE_AUTHORIZE {
				body, _ := proto.Marshal(&whalereinv1.PairingClaimResult{Authorized: false})
				return &whalereinv1.HostResult{Status: whalereinv1.OnlineStatus_succeeded, ResultBody: body}, nil
			}
			return defaultPairingAnswers(req)
		}
		if _, err := f.service.Claim(ctx, finishRequest(token, "claim", "d1")); !errors.Is(err, ErrHostRejected) {
			t.Fatalf("err = %v", err)
		}
		if f.store.grants["h1|d1"] {
			t.Fatalf("grant not compensated: %v", f.store.grants)
		}
		if _, used := f.store.used[hashKey(mustHash(token))]; used {
			t.Fatal("token marked used after failure")
		}
		if f.store.failures[hashKey(mustHash(token))] == 0 {
			t.Fatal("failure not counted")
		}
		assertUncommitted(t, f, token)
	})

	t.Run("preserves existing grant when authorization transport fails", func(t *testing.T) {
		f, token := base(t)
		fingerprint := hex.EncodeToString(sha256Sum(bytes.Repeat([]byte{4}, 32)))
		oldHash := sha256Sum([]byte("existing fixture credential"))
		if err := f.store.CreatePairedDevice(ctx, "h1", "d1", fingerprint, oldHash, f.now); err != nil {
			t.Fatal("existing authorization fixture could not be created")
		}
		f.transport.awaitFn = func(req *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
			var claim whalereinv1.PairingClaim
			_ = proto.Unmarshal(req.GetBody(), &claim)
			if claim.GetPhase() == whalereinv1.PairingPhase_PAIRING_PHASE_AUTHORIZE {
				return nil, errors.New("timeout")
			}
			return defaultPairingAnswers(req)
		}
		if _, err := f.service.Claim(ctx, finishRequest(token, "claim", "d1")); !errors.Is(err, ErrHostUnavailable) {
			t.Fatalf("err = %v", err)
		}
		if !f.store.grants["h1|d1"] || !bytes.Equal(f.store.devices["d1"].authHash, oldHash) || f.store.devices["d1"].fingerprint != fingerprint {
			t.Fatal("authorization transport failure changed the existing grant, credential, or pinned identity")
		}
		if _, used := f.store.used[hashKey(mustHash(token))]; used {
			t.Fatal("authorization transport failure consumed the repair invitation")
		}
	})

	t.Run("malformed authorization response persists no grant or invitation use", func(t *testing.T) {
		f, token := base(t)
		f.transport.awaitFn = func(req *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
			var claim whalereinv1.PairingClaim
			_ = proto.Unmarshal(req.GetBody(), &claim)
			if claim.GetPhase() == whalereinv1.PairingPhase_PAIRING_PHASE_AUTHORIZE {
				return &whalereinv1.HostResult{Status: whalereinv1.OnlineStatus_succeeded, ResultBody: []byte("bad")}, nil
			}
			return defaultPairingAnswers(req)
		}
		if _, err := f.service.Claim(ctx, finishRequest(token, "claim", "d1")); !errors.Is(err, ErrHostRejected) {
			t.Fatalf("err = %v", err)
		}
		assertUncommitted(t, f, token)
	})

	t.Run("failed atomic invitation write persists no device or grant and sends no activation", func(t *testing.T) {
		f, token := base(t)
		f.store.useErr = errors.New("db down")
		if _, err := f.service.Claim(ctx, finishRequest(token, "claim", "d1")); !errors.Is(err, ErrInternal) {
			t.Fatalf("err = %v", err)
		}
		assertUncommitted(t, f, token)
	})

	t.Run("rejects FINISH when invitation was already consumed", func(t *testing.T) {
		f, token := base(t)
		result := false
		f.store.useResult = &result
		if _, err := f.service.Claim(ctx, finishRequest(token, "claim", "d1")); !errors.Is(err, ErrTokenUsed) {
			t.Fatalf("err = %v", err)
		}
		assertUncommitted(t, f, token)
	})

	t.Run("rejects FINISH when device persistence fails", func(t *testing.T) {
		f, token := base(t)
		f.store.deviceErr = errors.New("db down")
		if _, err := f.service.Claim(ctx, finishRequest(token, "claim", "d1")); !errors.Is(err, ErrInternal) {
			t.Fatalf("err = %v", err)
		}
		if f.store.failures[hashKey(mustHash(token))] == 0 {
			t.Fatal("failure not counted")
		}
		assertUncommitted(t, f, token)
	})

	t.Run("optional host display metadata failure still commits authorization and invitation use", func(t *testing.T) {
		f, token := base(t)
		f.store.metaErr = errors.New("db down")
		response, err := f.service.Claim(ctx, finishRequest(token, "claim", "d1"))
		if err != nil {
			t.Fatalf("err = %v", err)
		}
		if response.GetDeviceToken() == "" || !f.store.grants["h1|d1"] || !bytes.Equal(f.store.devices["d1"].authHash, sha256Sum([]byte(response.GetDeviceToken()))) {
			t.Fatal("optional metadata failure lost committed authorization")
		}
		if _, used := f.store.used[hashKey(mustHash(token))]; !used {
			t.Fatal("successful FINISH left its invitation reusable")
		}
	})

	t.Run("rejects FINISH when secure token generation fails", func(t *testing.T) {
		f, token := base(t)
		f.service.random = bytes.NewReader(nil)
		if _, err := f.service.Claim(ctx, finishRequest(token, "claim", "d1")); !errors.Is(err, ErrInternal) {
			t.Fatalf("err = %v", err)
		}
		assertUncommitted(t, f, token)
	})
}

func TestFinishRequiresMatchingPriorHelloClaimState(t *testing.T) {
	f := newFixture(t)
	ctx := context.Background()
	_, token := f.invite(t)
	if _, err := f.service.Claim(ctx, helloRequest(token, "claim", "d1")); err != nil {
		t.Fatal(err)
	}
	// Wrong device for an existing claim id.
	if _, err := f.service.Claim(ctx, finishRequest(token, "claim", "d2")); !errors.Is(err, ErrClaimRequired) {
		t.Fatalf("wrong device = %v", err)
	}
	// Expired claim state.
	f.now = f.now.Add(2 * time.Minute)
	if _, err := f.service.Claim(ctx, finishRequest(token, "claim", "d1")); !errors.Is(err, ErrClaimRequired) {
		t.Fatalf("expired claim = %v", err)
	}
}

func TestFinishReturnsOfflineAndTransportErrorsWithoutGrant(t *testing.T) {
	ctx := context.Background()
	for name, setup := range map[string]func(*fixture){
		"rejects FINISH while host is offline":              func(f *fixture) { f.presence.online["h1"] = false },
		"rejects FINISH when host transport is unavailable": func(f *fixture) { f.transport.err = errors.New("mqtt down") },
	} {
		t.Run(name, func(t *testing.T) {
			f := newFixture(t)
			_, token := f.invite(t)
			if _, err := f.service.Claim(ctx, helloRequest(token, "claim", "d1")); err != nil {
				t.Fatal(err)
			}
			setup(f)
			_, err := f.service.Claim(ctx, finishRequest(token, "claim", "d1"))
			if err == nil {
				t.Fatal("expected error")
			}
			if len(f.store.devices) != 0 {
				t.Fatal("device written")
			}
		})
	}
}

func containsString(values []string, want string) bool {
	for _, value := range values {
		if value == want {
			return true
		}
	}
	return false
}

func TestRandomIDEncodesAllEighteenRandomBytesAndRejectsShortReads(t *testing.T) {
	id, err := randomID(bytes.NewReader(bytes.Repeat([]byte{1}, 18)))
	if err != nil || len(id) != 24 {
		t.Fatalf("id = %q err=%v", id, err)
	}
	decoded, err := base64.RawURLEncoding.DecodeString(id)
	if err != nil || !bytes.Equal(decoded, bytes.Repeat([]byte{1}, 18)) {
		t.Fatal("identifier changed or discarded the supplied random bytes")
	}
	other, err := randomID(bytes.NewReader(bytes.Repeat([]byte{2}, 18)))
	if err != nil || other == id {
		t.Fatal("distinct random inputs produced the same identifier")
	}
	for _, size := range []int{0, 1, 17} {
		t.Run(fmt.Sprintf("rejects_%d_of_18_random_bytes", size), func(t *testing.T) {
			if _, err := randomID(bytes.NewReader(make([]byte, size))); err == nil {
				t.Fatal("short randomness read unexpectedly succeeded")
			}
		})
	}
}

func TestPairingReplayAndCommitRecoveryPreserveIdentity(t *testing.T) {
	f := newFixture(t)
	_, token := f.invite(t)
	ctx := context.Background()
	hello := helloRequest(token, "recover", "d1")
	if _, err := f.service.Claim(ctx, hello); err != nil {
		t.Fatal(err)
	}
	before := len(f.transport.requests)
	if _, err := f.service.Claim(ctx, hello); err != nil || len(f.transport.requests) != before {
		t.Fatal("hello replay was not idempotent")
	}
	finish := finishRequest(token, "recover", "d1")
	reject := true
	f.transport.awaitFn = func(request *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
		var claim whalereinv1.PairingClaim
		_ = proto.Unmarshal(request.Body, &claim)
		if claim.Phase == whalereinv1.PairingPhase_PAIRING_PHASE_COMMIT && reject {
			reject = false
			return nil, errors.New("rejects FINISH while host is offline")
		}
		return defaultPairingAnswers(request)
	}
	if _, err := f.service.Claim(ctx, finish); !errors.Is(err, ErrHostUnavailable) {
		t.Fatal("commit failure not surfaced")
	}
	response, err := f.service.Claim(ctx, finish)
	if err != nil || response.DeviceId != "d1" {
		t.Fatal("commit retry failed")
	}
	before = len(f.transport.requests)
	replay, err := f.service.Claim(ctx, finish)
	if err != nil || !bytes.Equal(sha256Sum([]byte(replay.DeviceToken)), sha256Sum([]byte(response.DeviceToken))) || len(f.transport.requests) != before {
		t.Fatal("finish replay changed outcome")
	}
	wrong := proto.Clone(finish).(*whalereinv1.PairingClaimRequest)
	wrong.NoiseMessage = []byte("different-proof")
	if _, err := f.service.Claim(ctx, wrong); !errors.Is(err, ErrInvalidClaim) {
		t.Fatal("different finish proof reused receipt")
	}
	f.now = f.now.Add(2 * time.Minute)
	f.service.gcClaims(f.now)
	if len(f.service.completed) != 0 {
		t.Fatal("completed receipt expired incorrectly")
	}
}

func TestFailedRepairDoesNotRevokeExistingGrant(t *testing.T) {
	f := newFixture(t)
	_, token := f.invite(t)
	ctx := context.Background()
	if _, err := f.service.Claim(ctx, helloRequest(token, "repair", "d1")); err != nil {
		t.Fatal(err)
	}
	fp := sha256.Sum256(bytes.Repeat([]byte{4}, 32))
	if err := f.store.CreatePairedDevice(ctx, "h1", "d1", hex.EncodeToString(fp[:]), []byte("old"), f.now); err != nil {
		t.Fatal(err)
	}
	f.store.useErr = errors.New("db down")
	if _, err := f.service.Claim(ctx, finishRequest(token, "repair", "d1")); !errors.Is(err, ErrInternal) {
		t.Fatal("repair unexpectedly succeeded")
	}
	if !f.store.grants["h1|d1"] || !bytes.Equal(f.store.devices["d1"].authHash, []byte("old")) {
		t.Fatal("failed repair changed active authorization")
	}
}

func TestTokenSerializationCancelsWaitersAndReleasesIdleEntries(t *testing.T) {
	f := newFixture(t)
	release, err := f.service.lockToken(context.Background(), "token-scope")
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if _, err := f.service.lockToken(ctx, "token-scope"); !errors.Is(err, context.Canceled) {
		t.Fatal("canceled waiter retained")
	}
	release()
	f.service.locksMu.Lock()
	count := len(f.service.locks)
	f.service.locksMu.Unlock()
	if count != 0 {
		t.Fatal("idle token lock leaked")
	}
	next, err := f.service.lockToken(context.Background(), "token-scope")
	if err != nil {
		t.Fatal(err)
	}
	next()
}

func TestMalformedDevicePublicKeyCannotCreatePinnedIdentity(t *testing.T) {
	f := newFixture(t)
	_, token := f.invite(t)
	ctx := context.Background()
	if _, err := f.service.Claim(ctx, helloRequest(token, "bad-key", "d1")); err != nil {
		t.Fatal(err)
	}
	f.transport.awaitFn = func(request *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
		var claim whalereinv1.PairingClaim
		_ = proto.Unmarshal(request.Body, &claim)
		if claim.Phase == whalereinv1.PairingPhase_PAIRING_PHASE_FINISH {
			body, _ := proto.Marshal(&whalereinv1.PairingClaimResult{DeviceId: "d1", DevicePublicKey: []byte{1}})
			return &whalereinv1.HostResult{Status: whalereinv1.OnlineStatus_succeeded, ResultBody: body}, nil
		}
		return defaultPairingAnswers(request)
	}
	if _, err := f.service.Claim(ctx, finishRequest(token, "bad-key", "d1")); !errors.Is(err, ErrHostRejected) {
		t.Fatal("malformed key accepted")
	}
	if len(f.store.devices) != 0 {
		t.Fatal("malformed key pinned an identity")
	}
}
