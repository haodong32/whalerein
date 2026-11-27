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
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

type fakeStore struct {
	operations   map[string]store.PairingOperation
	outcomes     map[string]store.PairingOutcome
	outcomeErr   error
	confirmCalls int
	notices      map[string]store.Request
	hosts        map[string]store.Host
	tokens       map[string]store.PairingToken
	devices      map[string]struct {
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
	saveErr   error
}

func newFakeStore() *fakeStore {
	return &fakeStore{
		hosts:      map[string]store.Host{},
		operations: map[string]store.PairingOperation{}, outcomes: map[string]store.PairingOutcome{}, notices: map[string]store.Request{},
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

func (f *fakeStore) CommitPairing(ctx context.Context, tokenHash []byte, hostID, deviceID, fingerprint string, authHash []byte, at time.Time, notice *whalereinv1.HostRequest, operation *store.PairingOperation) (bool, error) {
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
	if err := f.CreatePairedDevice(ctx, hostID, deviceID, fingerprint, authHash, at); err != nil {
		return false, err
	}
	if operation != nil {
		op := f.operations[operation.ClaimID]
		op.FinishHash = operation.FinishHash
		op.FinishResponse = operation.FinishResponse
		op.CommitRequestID = notice.RequestId
		op.Active = true
		f.operations[op.ClaimID] = op
		var response whalereinv1.PairingClaimResponse
		if err := proto.Unmarshal(operation.FinishResponse, &response); err != nil {
			return false, err
		}
		f.outcomes[op.ClaimID] = store.PairingOutcome{ClaimID: op.ClaimID, HostID: hostID, DeviceID: deviceID, TokenHash: append([]byte(nil), tokenHash...), FinishHash: append([]byte(nil), operation.FinishHash...), DevicePublicKey: append([]byte(nil), response.DevicePublicKey...), ProtocolVersion: response.ProtocolVersion, Active: true}
		f.notices[notice.RequestId] = store.Request{RequestID: notice.RequestId, HostID: hostID, DeviceID: deviceID, Kind: notice.Kind,
			RequestBody: notice.Body, ProtocolVersion: notice.ProtocolVersion, ExpiresAt: time.UnixMilli(notice.ExpiresAtUnixMs)}
	}
	return true, nil
}

func (f *fakeStore) UpdateHostMetadata(_ context.Context, hostID, fingerprint string, protocolVersion uint32, _ time.Time) error {
	if f.metaErr != nil {
		return f.metaErr
	}
	f.meta = append(f.meta, hostID+":"+fingerprint+":"+string(rune('0'+protocolVersion)))
	return nil
}

type fakeTransport struct {
	capable  *bool
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

type fakePolicy struct {
	err   error
	calls []string
}

func (f *fakePolicy) ApplyPairing(_ context.Context, hostID, deviceID string) error {
	f.calls = append(f.calls, hostID+"|"+deviceID)
	return f.err
}

func (f *fakeTransport) SupportsPairing(string) bool { return f.capable == nil || *f.capable }

type fixture struct {
	service   *Service
	store     *fakeStore
	transport *fakeTransport
	presence  *fakePresence
	policy    *fakePolicy
	now       time.Time
}

func newFixture(t *testing.T) *fixture {
	t.Helper()
	fs := newFakeStore()
	fs.hosts["h1"] = store.Host{HostID: "h1"}
	transport := &fakeTransport{}
	transport.awaitFn = defaultPairingAnswers
	presence := &fakePresence{online: map[string]bool{"h1": true}}
	policy := &fakePolicy{}
	now := time.Date(2026, 11, 9, 12, 0, 0, 0, time.UTC)
	fixtureRef := &fixture{now: now}
	service, err := New(Dependencies{
		Store: fs, Policy: policy, Transport: transport, Presence: presence,
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
	fixtureRef.policy = policy
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
		result = whalereinv1.PairingClaimResult{DeviceId: claim.GetDeviceId(), DevicePublicKey: bytes.Repeat([]byte{4}, 32), DeviceAuthHash: claim.DeviceAuthHash}
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
		NoiseMessage: []byte("msg3"), DeviceId: deviceID, ProtocolVersion: 1, Confirmed: true, DeviceToken: base64.RawURLEncoding.EncodeToString(bytes.Repeat([]byte{8}, 32)),
	}
}

func TestNewRejectsMissingRequiredDependencies(t *testing.T) {
	if _, err := New(Dependencies{}); err == nil {
		t.Fatal("expected error")
	}
	if _, err := New(Dependencies{Store: newFakeStore()}); err == nil {
		t.Fatal("expected policy error")
	}
	if _, err := New(Dependencies{Store: newFakeStore(), Policy: &fakePolicy{}}); err == nil {
		t.Fatal("expected transport error")
	}
	if _, err := New(Dependencies{Store: newFakeStore(), Policy: &fakePolicy{}, Transport: &fakeTransport{}}); err == nil {
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
	if query.Get("exp") != strconv.FormatInt(f.now.Add(10*time.Minute).Unix(), 10) {
		t.Fatalf("exp = %q, want Unix seconds %d", query.Get("exp"), f.now.Add(10*time.Minute).Unix())
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

// The mobile invitation parser rejects an expired or malformed link locally,
// so the URI contract must carry a whole-second `exp` and a URL-safe token.
func TestInvitationURIExpiryUsesUnixSecondsForClientParsers(t *testing.T) {
	f := newFixture(t)
	invitation, err := f.service.CreateInvitation(context.Background(), "h1")
	if err != nil {
		t.Fatal(err)
	}
	uri, err := url.Parse(invitation.GetPairingUri())
	if err != nil {
		t.Fatal(err)
	}
	exp, err := strconv.ParseInt(uri.Query().Get("exp"), 10, 64)
	if err != nil {
		t.Fatalf("exp is not an integer: %v", err)
	}
	if exp != invitation.GetExpiresAtUnixMs()/1000 {
		t.Fatalf("exp = %d, expires_at_unix_ms = %d", exp, invitation.GetExpiresAtUnixMs())
	}
	for _, character := range invitation.GetToken() {
		if !(character >= 'a' && character <= 'z') && !(character >= 'A' && character <= 'Z') &&
			!(character >= '0' && character <= '9') && character != '-' && character != '_' && character != '.' && character != '~' {
			t.Fatalf("token contains a character mobile clients reject: %q", character)
		}
	}
}

// A consumed invitation must not reach the Host again and must never produce a
// second grant. Device B receives the deterministic used-token conflict; A
// remains the only paired device.
func TestConsumedInvitationRejectsSecondDeviceWithoutHostContact(t *testing.T) {
	f := newFixture(t)
	ctx := context.Background()
	_, token := f.invite(t)
	if _, err := f.service.Claim(ctx, helloRequest(token, "claim-a", "device-a")); err != nil {
		t.Fatal(err)
	}
	if _, err := f.service.Claim(ctx, finishRequest(token, "claim-a", "device-a")); err != nil {
		t.Fatal(err)
	}
	hostCalls := len(f.transport.requests)
	if _, err := f.service.Claim(ctx, helloRequest(token, "claim-b", "device-b")); !errors.Is(err, ErrTokenUsed) {
		t.Fatalf("second device = %v, want ErrTokenUsed", err)
	}
	if len(f.transport.requests) != hostCalls {
		t.Fatal("second device reached the Host with a consumed invitation")
	}
	if len(f.store.devices) != 1 || len(f.store.grants) != 1 {
		t.Fatalf("devices=%d grants=%d, want exactly one grant", len(f.store.devices), len(f.store.grants))
	}
	if _, ok := f.store.devices["device-a"]; !ok {
		t.Fatal("first device lost its grant")
	}
	if _, ok := f.store.devices["device-b"]; ok {
		t.Fatal("second device obtained a grant")
	}
}

// An unused invitation stays valid across Host metadata/key rotation. The
// fingerprint a client pins is the one the live Host proves during HELLO, not a
// snapshot taken when the invitation was minted. Admin-token rotation is
// likewise irrelevant: the stored invitation is bound to the Host, never to the
// bearer that minted it.
func TestUnusedInvitationSurvivesHostMetadataRotationUntilUse(t *testing.T) {
	f := newFixture(t)
	ctx := context.Background()
	_, token := f.invite(t)
	f.store.hosts["h1"] = store.Host{HostID: "h1", Fingerprint: "rotated-fingerprint"}
	hello, err := f.service.Claim(ctx, helloRequest(token, "claim-1", "device-a"))
	if err != nil {
		t.Fatal(err)
	}
	if hello.GetHostFingerprint() != "host-fp" {
		t.Fatalf("rotated Host fingerprint = %q, want the live HELLO proof", hello.GetHostFingerprint())
	}
	if _, err := f.service.Claim(ctx, finishRequest(token, "claim-1", "device-a")); err != nil {
		t.Fatal(err)
	}
	if len(f.store.devices) != 1 || len(f.store.grants) != 1 {
		t.Fatalf("devices=%d grants=%d, want one rotated grant", len(f.store.devices), len(f.store.grants))
	}
	if len(f.store.meta) != 1 || f.store.meta[0] != "h1:host-fp:1" {
		t.Fatalf("host metadata = %v", f.store.meta)
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
	if f.store.failures[hashKey(hash[:])] != 0 {
		t.Fatalf("an offline host consumed the failure budget: %d", f.store.failures[hashKey(hash[:])])
	}
	f.presence.online["h1"] = true
	f.transport.err = errors.New("mqtt down")
	if _, err := f.service.Claim(ctx, helloRequest(token, "c2", "d1")); !errors.Is(err, ErrHostUnavailable) {
		t.Fatalf("transport = %v", err)
	}
	if f.store.failures[hashKey(hash[:])] != 0 {
		t.Fatalf("an unreachable host consumed the failure budget: %d", f.store.failures[hashKey(hash[:])])
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
			before := f.store.failures[hashKey(hash[:])]
			if _, err := f.service.Claim(ctx, helloRequest(token, "cx", "d1")); err == nil {
				t.Fatal("expected error")
			}
			after := f.store.failures[hashKey(hash[:])]
			// A Host that reports failure without a proof rejection (status
			// failed) is unavailable, not a failed authentication.
			wantDelta := 0
			if name == "rejects HELLO when host reports rejected status" ||
				name == "rejects HELLO with malformed host response" ||
				name == "rejects HELLO response without Noise message" {
				wantDelta = 1
			}
			if after-before != wantDelta {
				t.Fatalf("failure delta = %d, want %d", after-before, wantDelta)
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

// The Host's COMMIT acknowledgement carries the exact Host-owned grant. Access
// must return it to the Device and keep it on the durable outcome so a retry
// after a lost response replays the same ID without a new Host round trip.
func TestFinishPersistsAndReplaysHostGrantID(t *testing.T) {
	const grantID = "grant_01HOSTOWNEDDEVICETRUSTACK"
	f := newFixture(t)
	ctx := context.Background()
	_, token := f.invite(t)
	answer := func(req *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
		var claim whalereinv1.PairingClaim
		if err := proto.Unmarshal(req.GetBody(), &claim); err != nil {
			return nil, err
		}
		result, err := defaultPairingAnswers(req)
		if err != nil {
			return nil, err
		}
		if claim.GetPhase() == whalereinv1.PairingPhase_PAIRING_PHASE_COMMIT {
			var ack whalereinv1.PairingClaimResult
			if err := proto.Unmarshal(result.GetResultBody(), &ack); err != nil {
				return nil, err
			}
			ack.GrantId = grantID
			result.ResultBody, err = proto.Marshal(&ack)
			if err != nil {
				return nil, err
			}
		}
		return result, nil
	}
	f.transport.awaitFn = answer
	if _, err := f.service.Claim(ctx, helloRequest(token, "grant-claim", "d1")); err != nil {
		t.Fatal(err)
	}
	response, err := f.service.Claim(ctx, finishRequest(token, "grant-claim", "d1"))
	if err != nil {
		t.Fatal(err)
	}
	if response.GetGrantId() != grantID {
		t.Fatalf("response grant = %q, want %q", response.GetGrantId(), grantID)
	}
	outcome := f.store.outcomes["grant-claim"]
	if outcome.GrantID != grantID || !outcome.HostConfirmed {
		t.Fatalf("durable outcome = %+v, want the confirmed Host grant", outcome)
	}

	// An exact FINISH retry must replay the durable grant without contacting
	// the Host again.
	hostCalls := len(f.transport.requests)
	retried, err := f.service.Claim(ctx, finishRequest(token, "grant-claim", "d1"))
	if err != nil {
		t.Fatal(err)
	}
	if retried.GetGrantId() != grantID {
		t.Fatalf("retry grant = %q, want %q", retried.GetGrantId(), grantID)
	}
	if len(f.transport.requests) != hostCalls {
		t.Fatalf("retry contacted the Host again: %d -> %d", hostCalls, len(f.transport.requests))
	}
	// A fresh service recovering from the same durable state returns the same
	// grant after a restart.
	restarted, err := New(Dependencies{Store: f.store, Policy: f.policy, Transport: f.transport,
		Presence: f.presence, Config: f.service.cfg, Now: func() time.Time { return f.now }})
	if err != nil {
		t.Fatal(err)
	}
	again, err := restarted.Claim(ctx, finishRequest(token, "grant-claim", "d1"))
	if err != nil || again.GetGrantId() != grantID {
		t.Fatalf("restart grant = %q err = %v", again.GetGrantId(), err)
	}
}

// A malformed Host grant identity is refused at the acknowledgement boundary
// instead of being persisted or returned to the Device.
func TestFinishRejectsMalformedHostGrantID(t *testing.T) {
	for _, grantID := range []string{"with space", "slash/id", strings.Repeat("a", 129), "grant\nline"} {
		t.Run(grantID[:min(len(grantID), 12)], func(t *testing.T) {
			f := newFixture(t)
			ctx := context.Background()
			_, token := f.invite(t)
			f.transport.awaitFn = func(req *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
				var claim whalereinv1.PairingClaim
				if err := proto.Unmarshal(req.GetBody(), &claim); err != nil {
					return nil, err
				}
				result, err := defaultPairingAnswers(req)
				if err != nil {
					return nil, err
				}
				if claim.GetPhase() == whalereinv1.PairingPhase_PAIRING_PHASE_COMMIT {
					var ack whalereinv1.PairingClaimResult
					if err := proto.Unmarshal(result.GetResultBody(), &ack); err != nil {
						return nil, err
					}
					ack.GrantId = grantID
					result.ResultBody, err = proto.Marshal(&ack)
					if err != nil {
						return nil, err
					}
				}
				return result, nil
			}
			if _, err := f.service.Claim(ctx, helloRequest(token, "bad-grant", "d1")); err != nil {
				t.Fatal(err)
			}
			if _, err := f.service.Claim(ctx, finishRequest(token, "bad-grant", "d1")); !errors.Is(err, ErrHostRejected) {
				t.Fatalf("grant %q error = %v, want ErrHostRejected", grantID, err)
			}
		})
	}
}

// An old Host that predates Host-owned grants returns an empty grant ID. Access
// must preserve that empty value without failing the pairing.
func TestFinishKeepsEmptyGrantIDForOldHost(t *testing.T) {
	f := newFixture(t)
	ctx := context.Background()
	_, token := f.invite(t)
	if _, err := f.service.Claim(ctx, helloRequest(token, "old-host", "d1")); err != nil {
		t.Fatal(err)
	}
	response, err := f.service.Claim(ctx, finishRequest(token, "old-host", "d1"))
	if err != nil {
		t.Fatal(err)
	}
	if response.GetGrantId() != "" {
		t.Fatalf("old Host grant = %q, want empty", response.GetGrantId())
	}
	if outcome := f.store.outcomes["old-host"]; outcome.GrantID != "" || !outcome.HostConfirmed {
		t.Fatalf("old Host outcome = %+v", outcome)
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
			t.Fatal("a Host authorization denial did not consume the failure budget")
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
		if f.store.failures[hashKey(mustHash(token))] != 0 {
			t.Fatal("a durable storage failure consumed the repair invitation budget")
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
	for id, r := range f.store.notices {
		r.Status = whalereinv1.OnlineStatus_succeeded
		r.ResultBody, _ = proto.Marshal(&whalereinv1.PairingClaimResult{Authorized: true, DeviceId: "d1"})
		f.store.notices[id] = r
	}
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
	if recovered, err := f.service.Claim(ctx, finish); err != nil || recovered.DeviceId != response.DeviceId {
		t.Fatal("verified durable outcome was not recoverable after operation expiry")
	}

}

func TestLegacyUnprovenActiveCommitIsNotRepublished(t *testing.T) {
	f := newFixture(t)
	_, token := f.invite(t)
	ctx := context.Background()
	if _, err := f.service.Claim(ctx, helloRequest(token, "unproven", "d1")); err != nil {
		t.Fatal(err)
	}
	finish := finishRequest(token, "unproven", "d1")
	if _, err := f.service.Claim(ctx, finish); err != nil {
		t.Fatal(err)
	}
	outcome := f.store.outcomes["unproven"]
	outcome.CommitBindingUnproven = true
	f.store.outcomes["unproven"] = outcome
	before := len(f.transport.requests)
	if _, err := f.service.Claim(ctx, finish); !errors.Is(err, ErrPairingCommittedUnconfirmed) {
		t.Fatal("unproven active outcome was not held unconfirmed")
	}
	if len(f.transport.requests) != before || !f.store.grants["h1|d1"] {
		t.Fatal("unproven outbox was republished or the grant was removed")
	}
	// An inactive exact FINISH is independently known unavailable; it does not
	// claim a prior Host activation and retains existing revocation semantics.
	f.store.grants["h1|d1"] = false
	if _, err := f.service.Claim(ctx, finish); !errors.Is(err, ErrPairingCommittedUnavailable) {
		t.Fatal("inactive exact FINISH lost unavailable semantics")
	}
	if len(f.transport.requests) != before {
		t.Fatal("inactive outbox was republished")
	}
}

func TestCommittedPairingRecoveryFailsClosedOnIncompleteEvidence(t *testing.T) {
	for _, tc := range []struct {
		name   string
		change func(*fixture)
		want   error
	}{
		{"outcome missing", func(f *fixture) { delete(f.store.outcomes, "proof") }, ErrPairingCommittedUnconfirmed},
		{"outcome store failure", func(f *fixture) { f.store.outcomeErr = errors.New("synthetic database failure") }, ErrInternal},
		{"token binding changed", func(f *fixture) {
			o := f.store.outcomes["proof"]
			o.TokenHash = bytes.Repeat([]byte{0}, 32)
			f.store.outcomes["proof"] = o
		}, ErrInvalidClaim},
		{"finish binding changed", func(f *fixture) {
			o := f.store.outcomes["proof"]
			o.FinishHash = bytes.Repeat([]byte{0}, 32)
			f.store.outcomes["proof"] = o
		}, ErrInvalidClaim},
		{"grant became inactive", func(f *fixture) { o := f.store.outcomes["proof"]; o.Active = false; f.store.outcomes["proof"] = o }, ErrPairingCommittedUnavailable},
		{"proof unavailable", func(f *fixture) {
			o := f.store.outcomes["proof"]
			o.CommitBindingUnproven = true
			f.store.outcomes["proof"] = o
		}, ErrPairingCommittedUnconfirmed},
		{"finish response damaged", func(f *fixture) {
			o := f.store.outcomes["proof"]
			o.HostConfirmed = false
			f.store.outcomes["proof"] = o
			op := f.store.operations["proof"]
			op.FinishResponse = []byte{0xff}
			f.store.operations["proof"] = op
		}, ErrInternal},
		{"outbox missing", func(f *fixture) {
			o := f.store.outcomes["proof"]
			o.HostConfirmed = false
			f.store.outcomes["proof"] = o
			delete(f.store.notices, f.store.operations["proof"].CommitRequestID)
		}, ErrPairingCommittedUnconfirmed},
		{"outbox body unavailable", func(f *fixture) {
			o := f.store.outcomes["proof"]
			o.HostConfirmed = false
			f.store.outcomes["proof"] = o
			id := f.store.operations["proof"].CommitRequestID
			n := f.store.notices[id]
			n.RequestBody = nil
			f.store.notices[id] = n
		}, ErrPairingCommittedUnconfirmed},
		{"outbox expired", func(f *fixture) {
			o := f.store.outcomes["proof"]
			o.HostConfirmed = false
			f.store.outcomes["proof"] = o
			id := f.store.operations["proof"].CommitRequestID
			n := f.store.notices[id]
			n.ExpiresAt = f.now
			f.store.notices[id] = n
		}, ErrPairingCommittedUnconfirmed},
	} {
		t.Run(tc.name, func(t *testing.T) {
			f := newFixture(t)
			_, token := f.invite(t)
			ctx := context.Background()
			if _, err := f.service.Claim(ctx, helloRequest(token, "proof", "d1")); err != nil {
				t.Fatal(err)
			}
			finish := finishRequest(token, "proof", "d1")
			if _, err := f.service.Claim(ctx, finish); err != nil {
				t.Fatal(err)
			}
			before := len(f.transport.requests)
			tc.change(f)
			if _, err := f.service.Claim(ctx, finish); !errors.Is(err, tc.want) {
				t.Fatalf("unexpected recovery error: %v", err)
			}
			if len(f.transport.requests) != before || !f.store.grants["h1|d1"] {
				t.Fatal("unproven recovery republished or changed the grant")
			}
		})
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

func (f *fakeStore) PairingGenerations(ctx context.Context, hash []byte, _ string) (int64, int64, error) {
	_, err := f.PairingTokenByHash(ctx, hash)
	return 0, 0, err
}

func (f *fakeStore) ConfirmPairingHost(_ context.Context, claimID, _, _, _, grantID string, _ time.Time) (bool, error) {
	f.confirmCalls++
	if outcome, ok := f.outcomes[claimID]; ok {
		outcome.HostConfirmed = true
		if grantID != "" {
			outcome.GrantID = grantID
		}
		f.outcomes[claimID] = outcome
	}
	return true, nil
}

func (f *fakeStore) PairingOutcome(_ context.Context, claimID, deviceID string) (store.PairingOutcome, error) {
	if f.outcomeErr != nil {
		return store.PairingOutcome{}, f.outcomeErr
	}
	value, ok := f.outcomes[claimID]
	if !ok || value.DeviceID != deviceID {
		return store.PairingOutcome{}, store.ErrNotFound
	}
	return value, nil
}

func (f *fakeStore) PairingFenced(_ context.Context, _, _, _ string) (bool, error) { return false, nil }

func (f *fakeStore) SavePairingOperation(_ context.Context, op store.PairingOperation) error {
	if f.saveErr != nil {
		return f.saveErr
	}
	if _, ok := f.operations[op.ClaimID]; ok {
		return store.ErrConflict
	}
	f.operations[op.ClaimID] = op
	return nil
}
func (f *fakeStore) PairingOperation(_ context.Context, id string) (store.PairingOperation, error) {
	op, ok := f.operations[id]
	if !ok {
		return op, store.ErrNotFound
	}
	if len(op.FinishHash) > 0 {
		op.Active = f.grants[op.HostID+"|"+op.DeviceID]
	}
	return op, nil
}
func (f *fakeStore) GetRequest(_ context.Context, id string) (store.Request, error) {
	r, ok := f.notices[id]
	if !ok {
		return r, store.ErrNotFound
	}
	return r, nil
}

func TestDurablePairingRecoversAfterServiceRestartWithoutPersistingCredential(t *testing.T) {
	f := newFixture(t)
	ctx := context.Background()
	_, token := f.invite(t)
	hello := helloRequest(token, "restart", "d1")
	if _, err := f.service.Claim(ctx, hello); err != nil {
		t.Fatal(err)
	}
	// A fresh service loses every in-memory claim and waiter.
	restarted, err := New(Dependencies{Store: f.store, Policy: f.policy, Transport: f.transport, Presence: f.presence, Config: f.service.cfg, Now: func() time.Time { return f.now }, Random: bytes.NewReader(bytes.Repeat([]byte{9}, 1024))})
	if err != nil {
		t.Fatal(err)
	}
	finish := finishRequest(token, "restart", "d1")
	reply, err := restarted.Claim(ctx, finish)
	if err != nil {
		t.Fatal(err)
	}
	op := f.store.operations["restart"]
	if bytes.Contains(op.FinishResponse, []byte(finish.DeviceToken)) || bytes.Contains(op.HelloResponse, []byte(token)) {
		t.Fatal("credential persisted in operation")
	}
	restarted, err = New(Dependencies{Store: f.store, Policy: f.policy, Transport: f.transport, Presence: f.presence, Config: f.service.cfg, Now: func() time.Time { return f.now }})
	if err != nil {
		t.Fatal(err)
	}
	again, err := restarted.Claim(ctx, finish)
	if err != nil || !proto.Equal(reply, again) {
		t.Fatal("committed pairing could not be recovered")
	}
	changed := proto.Clone(finish).(*whalereinv1.PairingClaimRequest)
	changed.DeviceToken = base64.RawURLEncoding.EncodeToString(bytes.Repeat([]byte{2}, 32))
	if _, err := restarted.Claim(ctx, changed); !errors.Is(err, ErrInvalidClaim) {
		t.Fatal("changed credential reused receipt")
	}
	f.store.grants["h1|d1"] = false
	if _, err := restarted.Claim(ctx, finish); !errors.Is(err, ErrPairingCommittedUnavailable) {
		t.Fatal("committed unavailable pairing was not identified")
	}
	if _, err := restarted.Claim(ctx, changed); !errors.Is(err, ErrInvalidClaim) {
		t.Fatal("changed FINISH gained committed proof")
	}
}

func TestFinishRejectsMissingCredentialAndHostCredentialSubstitution(t *testing.T) {
	f := newFixture(t)
	ctx := context.Background()
	_, token := f.invite(t)
	request := finishRequest(token, "bound", "d1")
	request.DeviceToken = ""
	if _, err := f.service.Claim(ctx, request); !errors.Is(err, ErrInvalidClaim) {
		t.Fatal("missing credential accepted")
	}
	request.DeviceToken = "invalid"
	if _, err := f.service.Claim(ctx, request); !errors.Is(err, ErrInvalidClaim) {
		t.Fatal("invalid credential accepted")
	}
	if _, err := f.service.Claim(ctx, helloRequest(token, "bound", "d1")); err != nil {
		t.Fatal(err)
	}
	f.transport.awaitFn = func(req *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
		response, err := defaultPairingAnswers(req)
		var result whalereinv1.PairingClaimResult
		_ = proto.Unmarshal(response.ResultBody, &result)
		result.DeviceAuthHash = nil
		response.ResultBody, _ = proto.Marshal(&result)
		return response, err
	}
	if _, err := f.service.Claim(ctx, finishRequest(token, "bound", "d1")); !errors.Is(err, ErrHostRejected) {
		t.Fatal("Host did not authenticate credential")
	}
	if f.store.grants["h1|d1"] {
		t.Fatal("substitution created grant")
	}
}

func TestHelloBootstrapsCapabilityButRejectsAnUnsupportedHost(t *testing.T) {
	f := newFixture(t)
	_, token := f.invite(t)
	ctx := context.Background()
	capable := false
	f.transport.capable = &capable
	f.transport.awaitFn = func(request *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
		capable = true
		return defaultPairingAnswers(request)
	}
	if _, err := f.service.Claim(ctx, helloRequest(token, "bootstrap", "d1")); err != nil {
		t.Fatal("unknown capability could not bootstrap")
	}
	capable = false
	f.transport.awaitFn = defaultPairingAnswers
	if _, err := f.service.Claim(ctx, helloRequest(token, "legacy", "d2")); !errors.Is(err, ErrProtocolVersion) {
		t.Fatal("unsupported Host accepted")
	}
	if len(f.store.grants) != 0 {
		t.Fatal("HELLO activated a grant")
	}
}

type delayedHostFenceStore struct {
	*fakeStore
	captured   chan struct{}
	generation int64
}

func (f *delayedHostFenceStore) PairingGenerations(_ context.Context, _ []byte, _ string) (int64, int64, error) {
	snapshot := f.generation
	close(f.captured)
	return 0, snapshot, nil
}
func (f *delayedHostFenceStore) SavePairingOperation(ctx context.Context, op store.PairingOperation) error {
	if op.HostGeneration != f.generation {
		return store.ErrPairingFenced
	}
	return f.fakeStore.SavePairingOperation(ctx, op)
}

// An already-arrived HELLO must not adopt a Host revocation that commits while it waits for its token lock.
func TestPairingHelloWaitingOnTokenLockCannotAdoptLaterHostRevoke(t *testing.T) {
	f := newFixture(t)
	_, token := f.invite(t)
	observed := &delayedHostFenceStore{fakeStore: f.store, captured: make(chan struct{})}
	f.service.store = observed
	digest := sha256.Sum256([]byte(token))
	unlock, err := f.service.lockToken(context.Background(), hex.EncodeToString(digest[:]))
	if err != nil {
		t.Fatal(err)
	}
	result := make(chan error, 1)
	go func() {
		_, err := f.service.Claim(context.Background(), helloRequest(token, "held-hello", "d1"))
		result <- err
	}()
	<-observed.captured
	observed.generation = 1
	unlock()
	if err := <-result; !errors.Is(err, ErrPairingFenced) {
		t.Fatal("HELLO adopted a later Host revocation")
	}
}

// Cleaned outcomes require an exact FINISH and do not claim Host activation.
func TestCleanedPairingOutcomeAuthenticatesExactFinishAndDistinguishesState(t *testing.T) {
	f := newFixture(t)
	_, token := f.invite(t)
	finish := finishRequest(token, "outcome", "d1")
	hash := sha256.Sum256([]byte(token))
	outcome := store.PairingOutcome{ClaimID: "outcome", HostID: "h1", DeviceID: "d1", TokenHash: hash[:], FinishHash: claimDigest(finish), Active: true}
	f.store.outcomes[outcome.ClaimID] = outcome
	if _, err := f.service.Claim(context.Background(), finish); !errors.Is(err, ErrPairingCommittedUnconfirmed) {
		t.Fatal("active outcome falsely confirmed or lost")
	}
	outcome.HostConfirmed = true
	outcome.DevicePublicKey = bytes.Repeat([]byte{4}, 32)
	outcome.ProtocolVersion = finish.ProtocolVersion
	f.store.outcomes[outcome.ClaimID] = outcome
	recovered, err := f.service.Claim(context.Background(), finish)
	if err != nil || recovered.HostId != "h1" || recovered.DeviceId != "d1" || recovered.DeviceToken != finish.DeviceToken || !bytes.Equal(recovered.DevicePublicKey, outcome.DevicePublicKey) {
		t.Fatal("confirmed active outcome did not recover exact binding")
	}
	outcome.CommitBindingUnproven = true
	f.store.outcomes[outcome.ClaimID] = outcome
	if _, err := f.service.Claim(context.Background(), finish); !errors.Is(err, ErrPairingCommittedUnconfirmed) {
		t.Fatal("unproven active legacy flag was trusted")
	}
	outcome.Active = false
	f.store.outcomes[outcome.ClaimID] = outcome
	if _, err := f.service.Claim(context.Background(), finish); !errors.Is(err, ErrPairingCommittedUnavailable) {
		t.Fatal("unavailable outcome not distinguished")
	}
	changed := proto.Clone(finish).(*whalereinv1.PairingClaimRequest)
	changed.DeviceToken = base64.RawURLEncoding.EncodeToString(bytes.Repeat([]byte{3}, 32))
	if _, err := f.service.Claim(context.Background(), changed); !errors.Is(err, ErrInvalidClaim) {
		t.Fatal("changed FINISH authenticated")
	}
	changed = proto.Clone(finish).(*whalereinv1.PairingClaimRequest)
	changed.Token = "different-token"
	if _, err := f.service.Claim(context.Background(), changed); !errors.Is(err, ErrInvalidClaim) {
		t.Fatal("changed token authenticated")
	}
	changed = proto.Clone(finish).(*whalereinv1.PairingClaimRequest)
	changed.DeviceId = "other-device"
	if _, err := f.service.Claim(context.Background(), changed); errors.Is(err, ErrPairingCommittedUnavailable) || errors.Is(err, ErrPairingCommittedUnconfirmed) {
		t.Fatal("different device obtained proof")
	}
	f.store.outcomeErr = errors.New("fixture storage failure")
	if _, err := f.service.Claim(context.Background(), finish); !errors.Is(err, ErrInternal) {
		t.Fatal("storage error treated as proof")
	}
}

// Host COMMIT evidence must match the authorized result and the original Host, device and request.
func TestHostCommitConfirmationRejectsMismatchedOrFailedAcknowledgement(t *testing.T) {
	for _, variant := range []string{"status", "host", "device", "request", "body-device"} {
		t.Run(variant, func(t *testing.T) {
			f := newFixture(t)
			claim := &whalereinv1.PairingClaim{ClaimId: "claim", DeviceId: "d1", Phase: whalereinv1.PairingPhase_PAIRING_PHASE_COMMIT}
			body, _ := proto.Marshal(claim)
			commit := &whalereinv1.HostRequest{RequestId: "commit", HostId: "h1", DeviceId: "d1", Kind: whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE, Body: body}
			done := &completedClaim{response: &whalereinv1.PairingClaimResponse{ClaimId: "claim", HostId: "h1", DeviceId: "d1"}, commit: commit}
			f.transport.awaitFn = func(req *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
				result, err := defaultPairingAnswers(req)
				if err != nil {
					return nil, err
				}
				switch variant {
				case "status":
					result.Status = whalereinv1.OnlineStatus_failed
				case "host":
					result.HostId = "other"
				case "device":
					result.DeviceId = "other"
				case "request":
					result.RequestId = "other"
				case "body-device":
					result.ResultBody, _ = proto.Marshal(&whalereinv1.PairingClaimResult{Authorized: true, DeviceId: "other"})
				}
				return result, nil
			}
			if _, err := f.service.completeClaim(context.Background(), done); !errors.Is(err, ErrHostRejected) {
				t.Fatal("invalid acknowledgement accepted")
			}
			if f.store.confirmCalls != 0 {
				t.Fatal("invalid acknowledgement persisted")
			}
		})
	}
}

func TestSuccessfulFinishAppliesPairingPolicyBeforeReturning(t *testing.T) {
	f := newFixture(t)
	ctx := context.Background()
	_, token := f.invite(t)
	if _, err := f.service.Claim(ctx, helloRequest(token, "policy-ok", "d1")); err != nil {
		t.Fatal(err)
	}
	if len(f.policy.calls) != 0 {
		t.Fatalf("policy applied before FINISH: %v", f.policy.calls)
	}
	if _, err := f.service.Claim(ctx, finishRequest(token, "policy-ok", "d1")); err != nil {
		t.Fatal(err)
	}
	if len(f.policy.calls) != 1 || f.policy.calls[0] != "h1|d1" {
		t.Fatalf("policy calls = %v", f.policy.calls)
	}
}

func TestPairingPolicyFailureReturnsInternalAfterDurableCommit(t *testing.T) {
	f := newFixture(t)
	ctx := context.Background()
	_, token := f.invite(t)
	if _, err := f.service.Claim(ctx, helloRequest(token, "policy-fail", "d1")); err != nil {
		t.Fatal(err)
	}
	f.policy.err = errors.New("broker policy unavailable")
	reply, err := f.service.Claim(ctx, finishRequest(token, "policy-fail", "d1"))
	if !errors.Is(err, ErrInternal) || reply != nil {
		t.Fatalf("reply = %+v err = %v", reply, err)
	}
	if len(f.policy.calls) != 1 {
		t.Fatalf("policy calls = %v", f.policy.calls)
	}
	// The durable commit already happened; only the response was withheld.
	if !f.store.grants["h1|d1"] {
		t.Fatal("durable grant missing after policy failure")
	}
	if _, ok := f.store.devices["d1"]; !ok {
		t.Fatal("durable device missing after policy failure")
	}
	if _, used := f.store.used[hashKey(mustHash(token))]; !used {
		t.Fatal("durable token use missing after policy failure")
	}
	if _, ok := f.store.operations["policy-fail"]; !ok {
		t.Fatal("durable operation missing after policy failure")
	}
}
