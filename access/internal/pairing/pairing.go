// Package pairing owns one-time invitations and first registration.
//
// HELLO and FINISH pairing claims are relayed to the connected host as
// transient MQTT HostRequests with in-memory correlation only. No requests row
// is written. devices and grants are written only after the host confirms
// FINISH and before PAIRING_AUTHORIZE.
package pairing

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/url"
	"strings"
	"sync"
	"time"

	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

// Stable pairing failures.
var (
	ErrUnauthorized    = errors.New("invalid pairing token")
	ErrTokenUsed       = errors.New("pairing token already used")
	ErrTokenExpired    = errors.New("pairing token expired")
	ErrFailureBudget   = errors.New("pairing token failure budget exhausted")
	ErrHostOffline     = errors.New("host offline")
	ErrHostRejected    = errors.New("host rejected pairing")
	ErrHostUnavailable = errors.New("host unavailable")
	ErrClaimRequired   = errors.New("pairing HELLO is required before FINISH")
	ErrInvalidClaim    = errors.New("invalid pairing claim")
	ErrProtocolVersion = errors.New("protocol version incompatible")
	ErrHostNotFound    = errors.New("host not found")
	ErrInternal        = errors.New("pairing internal error")
)

// Store is the pairing persistence boundary.
type Store interface {
	GetHost(ctx context.Context, hostID string) (store.Host, error)
	CreatePairingToken(ctx context.Context, tokenHash []byte, hostID string, expiresAt time.Time, at time.Time) error
	PairingTokenByHash(ctx context.Context, tokenHash []byte) (store.PairingToken, error)
	IncrementPairingFailure(ctx context.Context, tokenHash []byte, at time.Time) (int, error)
	CommitPairing(ctx context.Context, tokenHash []byte, hostID, deviceID, fingerprint string, authHash []byte, at time.Time, notice *whalereinv1.HostRequest) (bool, error)
	UpdateHostMetadata(ctx context.Context, hostID, fingerprint string, protocolVersion uint32, at time.Time) error
}

// Transport relays transient HostRequests and waits for the matching result.
type Transport interface {
	SupportsPairing(hostID string) bool
	PublishAndAwait(ctx context.Context, hostID string, req *whalereinv1.HostRequest, timeout time.Duration) (*whalereinv1.HostResult, error)
}

// Presence is broker-derived host connectivity.
type Presence interface {
	IsHostOnline(ctx context.Context, hostID string) bool
}

// Config holds invitation and claim tunables.
type Config struct {
	PublicURL       string
	MQTTURL         string
	InvitationTTL   time.Duration
	ClaimTimeout    time.Duration
	ClaimTTL        time.Duration
	MaxFailures     int
	ProtocolVersion uint32
}

// Dependencies wires the pairing service.
type Dependencies struct {
	Store     Store
	Transport Transport
	Presence  Presence
	Config    Config
	Metrics   *diagnostics.Metrics
	Masker    *diagnostics.Masker
	Logger    *slog.Logger
	Now       func() time.Time
	Random    io.Reader
}

type claimState struct {
	tokenHash       []byte
	tokenHashHex    string
	deviceID        string
	hostFingerprint string
	protocolVersion uint32
	expiresAt       time.Time
	helloProof      [32]byte
	helloReply      *whalereinv1.PairingClaimResponse
}
type completedClaim struct {
	tokenHash [32]byte
	proof     [32]byte
	response  *whalereinv1.PairingClaimResponse
	commit    *whalereinv1.HostRequest
	expiresAt time.Time
	ready     bool
}

// Service implements invitation and claim flows.
type Service struct {
	store     Store
	transport Transport
	presence  Presence
	cfg       Config
	metrics   *diagnostics.Metrics
	masker    *diagnostics.Masker
	logger    *slog.Logger
	now       func() time.Time
	random    io.Reader

	claimsMu  sync.Mutex
	claims    map[string]claimState
	completed map[string]*completedClaim

	locksMu sync.Mutex
	locks   map[string]*tokenLock
}

// New constructs a pairing service.
func New(deps Dependencies) (*Service, error) {
	if deps.Store == nil {
		return nil, errors.New("pairing: store is required")
	}
	if deps.Transport == nil {
		return nil, errors.New("pairing: transport is required")
	}
	if deps.Presence == nil {
		return nil, errors.New("pairing: presence is required")
	}
	if deps.Now == nil {
		deps.Now = func() time.Time { return time.Now().UTC() }
	}
	if deps.Logger == nil {
		deps.Logger = slog.Default()
	}
	if deps.Metrics == nil {
		deps.Metrics = diagnostics.NewMetrics()
	}
	if deps.Masker == nil {
		deps.Masker = diagnostics.NewMasker()
	}
	if deps.Random == nil {
		deps.Random = rand.Reader
	}
	return &Service{
		store:     deps.Store,
		transport: deps.Transport,
		presence:  deps.Presence,
		cfg:       deps.Config,
		metrics:   deps.Metrics,
		masker:    deps.Masker,
		logger:    deps.Logger,
		now:       deps.Now,
		random:    deps.Random,
		claims:    make(map[string]claimState),
		completed: make(map[string]*completedClaim),
		locks:     make(map[string]*tokenLock),
	}, nil
}

// CreateInvitation creates a one-time token bound to one host. The database
// stores only SHA-256(token).
func (s *Service) CreateInvitation(ctx context.Context, hostID string) (*whalereinv1.PairingInvitation, error) {
	if strings.TrimSpace(hostID) == "" {
		return nil, ErrInvalidClaim
	}
	if _, err := s.store.GetHost(ctx, hostID); err != nil {
		if errors.Is(err, store.ErrNotFound) {
			return nil, ErrHostNotFound
		}
		return nil, fmt.Errorf("lookup host: %w", err)
	}
	raw := make([]byte, 32)
	if _, err := io.ReadFull(s.random, raw); err != nil {
		return nil, fmt.Errorf("generate pairing token: %w", err)
	}
	token := base64.RawURLEncoding.EncodeToString(raw)
	hash := sha256.Sum256([]byte(token))
	now := s.now()
	expires := now.Add(s.cfg.InvitationTTL)
	if err := s.store.CreatePairingToken(ctx, hash[:], hostID, expires, now); err != nil {
		return nil, fmt.Errorf("create pairing token: %w", err)
	}
	uri := fmt.Sprintf("whalerein://pair?v=1&access=%s",
		url.QueryEscape(strings.TrimRight(s.cfg.PublicURL, "/")))
	if s.cfg.MQTTURL != "" {
		uri += "&mqtt=" + url.QueryEscape(s.cfg.MQTTURL)
	}
	uri += "&host=" + url.QueryEscape(hostID) + "&token=" + url.QueryEscape(token)
	s.logger.Info("pairing invitation created",
		"host", s.masker.Host(hostID), "expires_at", expires.Format(time.RFC3339))
	return &whalereinv1.PairingInvitation{
		Token:           token,
		HostId:          hostID,
		ExpiresAtUnixMs: expires.UnixMilli(),
		PairingUri:      uri,
	}, nil
}

// Claim runs one public pairing step. The token is revalidated before every
// step and any failure consumes one unit of the token's failure budget.
func (s *Service) Claim(ctx context.Context, request *whalereinv1.PairingClaimRequest) (*whalereinv1.PairingClaimResponse, error) {
	if request == nil {
		return nil, ErrInvalidClaim
	}
	if request.GetToken() == "" || len(request.Token) > 4096 || request.GetClaimId() == "" || len(request.ClaimId) > 128 || request.GetDeviceId() == "" || len(request.DeviceId) > 128 || len(request.NoiseMessage) > 64*1024 {
		return nil, ErrInvalidClaim
	}
	if request.GetProtocolVersion() != s.cfg.ProtocolVersion {
		return nil, ErrProtocolVersion
	}
	phase := request.GetPhase()
	if phase != whalereinv1.PairingPhase_PAIRING_PHASE_HELLO && phase != whalereinv1.PairingPhase_PAIRING_PHASE_FINISH {
		return nil, fmt.Errorf("%w: phase is not accepted over HTTP", ErrInvalidClaim)
	}
	if len(request.GetNoiseMessage()) == 0 {
		return nil, fmt.Errorf("%w: noise_message is required", ErrInvalidClaim)
	}
	if phase == whalereinv1.PairingPhase_PAIRING_PHASE_FINISH && !request.GetConfirmed() {
		return nil, fmt.Errorf("%w: fingerprint confirmation is required", ErrInvalidClaim)
	}

	hash := sha256.Sum256([]byte(request.GetToken()))
	hashHex := hex.EncodeToString(hash[:])
	now := s.now()

	unlock, lockErr := s.lockToken(ctx, hashHex)
	if lockErr != nil {
		return nil, lockErr
	}
	defer unlock()

	s.gcClaims(now)
	proof := sha256.Sum256(request.NoiseMessage)
	s.claimsMu.Lock()
	done := s.completed[request.ClaimId]
	previous, known := s.claims[request.ClaimId]
	budget := len(s.claims) + len(s.completed)
	s.claimsMu.Unlock()
	if phase == whalereinv1.PairingPhase_PAIRING_PHASE_FINISH && done != nil {
		if done.tokenHash != hash || done.proof != proof || done.response.DeviceId != request.DeviceId {
			return nil, ErrInvalidClaim
		}
		return s.completeClaim(ctx, done)
	}
	if phase == whalereinv1.PairingPhase_PAIRING_PHASE_HELLO && known && previous.tokenHashHex == hashHex && previous.deviceID == request.DeviceId && previous.helloProof == proof && previous.helloReply != nil {
		return proto.Clone(previous.helloReply).(*whalereinv1.PairingClaimResponse), nil
	}
	if budget >= 1024 {
		return nil, ErrFailureBudget
	}
	token, err := s.store.PairingTokenByHash(ctx, hash[:])
	if err != nil {
		if errors.Is(err, store.ErrNotFound) {
			return nil, ErrUnauthorized
		}
		return nil, fmt.Errorf("lookup pairing token: %w", err)
	}
	if token.UsedAt != nil {
		return nil, ErrTokenUsed
	}
	if !now.Before(token.ExpiresAt) {
		return nil, ErrTokenExpired
	}
	if s.cfg.MaxFailures > 0 && token.FailureCount >= s.cfg.MaxFailures {
		return nil, ErrFailureBudget
	}
	s.gcClaims(now)

	if phase == whalereinv1.PairingPhase_PAIRING_PHASE_HELLO {
		return s.hello(ctx, request, token, hashHex, now)
	}
	return s.finish(ctx, request, token, hashHex, now)
}

func (s *Service) hello(ctx context.Context, request *whalereinv1.PairingClaimRequest, token store.PairingToken, hashHex string, now time.Time) (*whalereinv1.PairingClaimResponse, error) {

	if !s.presence.IsHostOnline(ctx, token.HostID) {
		s.recordFailure(ctx, token.TokenHash, now)
		return nil, ErrHostOffline
	}
	if !s.transport.SupportsPairing(token.HostID) {
		return nil, ErrProtocolVersion
	}
	claim := &whalereinv1.PairingClaim{
		ClaimId:         request.GetClaimId(),
		TokenHash:       hashHex,
		Phase:           whalereinv1.PairingPhase_PAIRING_PHASE_HELLO,
		NoiseMessage:    request.GetNoiseMessage(),
		DeviceId:        request.GetDeviceId(),
		ExpiresAtUnixMs: token.ExpiresAt.UnixMilli(),
	}
	result, err := s.forward(ctx, token.HostID, whalereinv1.RequestKind_REQUEST_KIND_PAIRING_CLAIM, claim, now)
	if err != nil {
		s.recordFailure(ctx, token.TokenHash, now)
		return nil, err
	}
	claimResult, err := decodePairingResult(result)
	if err != nil || len(claimResult.GetNoiseMessage()) == 0 {
		s.recordFailure(ctx, token.TokenHash, now)
		return nil, ErrHostRejected
	}

	s.claimsMu.Lock()
	s.claims[request.GetClaimId()] = claimState{
		tokenHash:       append([]byte(nil), token.TokenHash...),
		tokenHashHex:    hashHex,
		deviceID:        request.GetDeviceId(),
		hostFingerprint: claimResult.GetHostFingerprint(),
		protocolVersion: request.GetProtocolVersion(),
		expiresAt:       now.Add(s.cfg.ClaimTTL),
	}
	s.claimsMu.Unlock()

	s.logger.Info("pairing hello relayed",
		"host", s.masker.Host(token.HostID), "device", s.masker.Device(request.GetDeviceId()))
	reply := &whalereinv1.PairingClaimResponse{
		ClaimId:         request.GetClaimId(),
		HostId:          token.HostID,
		Phase:           whalereinv1.PairingPhase_PAIRING_PHASE_HELLO,
		NoiseMessage:    claimResult.GetNoiseMessage(),
		HostPublicKey:   claimResult.GetHostPublicKey(),
		HostFingerprint: claimResult.GetHostFingerprint(),
		DeviceId:        request.GetDeviceId(),
		ExpiresAtUnixMs: token.ExpiresAt.UnixMilli(),
		ProtocolVersion: request.GetProtocolVersion(),
	}
	s.claimsMu.Lock()
	cached := s.claims[request.ClaimId]
	cached.helloProof = sha256.Sum256(request.NoiseMessage)
	cached.helloReply = reply
	s.claims[request.ClaimId] = cached
	s.claimsMu.Unlock()
	return proto.Clone(reply).(*whalereinv1.PairingClaimResponse), nil

}

func (s *Service) finish(ctx context.Context, request *whalereinv1.PairingClaimRequest, token store.PairingToken, hashHex string, now time.Time) (*whalereinv1.PairingClaimResponse, error) {
	s.claimsMu.Lock()
	state, ok := s.claims[request.GetClaimId()]
	s.claimsMu.Unlock()
	if !ok || state.tokenHashHex != hashHex || state.deviceID != request.GetDeviceId() || !now.Before(state.expiresAt) {
		return nil, ErrClaimRequired
	}
	if !s.presence.IsHostOnline(ctx, token.HostID) {
		s.recordFailure(ctx, token.TokenHash, now)
		return nil, ErrHostOffline
	}

	claim := &whalereinv1.PairingClaim{
		ClaimId:         request.GetClaimId(),
		TokenHash:       hashHex,
		Phase:           whalereinv1.PairingPhase_PAIRING_PHASE_FINISH,
		NoiseMessage:    request.GetNoiseMessage(),
		DeviceId:        request.GetDeviceId(),
		ExpiresAtUnixMs: token.ExpiresAt.UnixMilli(),
	}
	result, err := s.forward(ctx, token.HostID, whalereinv1.RequestKind_REQUEST_KIND_PAIRING_CLAIM, claim, now)
	if err != nil {
		s.recordFailure(ctx, token.TokenHash, now)
		return nil, err
	}
	claimResult, err := decodePairingResult(result)
	if err != nil {
		s.recordFailure(ctx, token.TokenHash, now)
		return nil, ErrHostRejected
	}
	if claimResult.GetDeviceId() != "" && claimResult.GetDeviceId() != request.GetDeviceId() {
		s.recordFailure(ctx, token.TokenHash, now)
		return nil, ErrHostRejected
	}
	if len(claimResult.GetDevicePublicKey()) != 32 {
		s.recordFailure(ctx, token.TokenHash, now)
		return nil, ErrHostRejected
	}

	deviceToken, authHash, err := s.newDeviceToken()
	if err != nil {
		return nil, err
	}
	fingerprint := sha256.Sum256(claimResult.GetDevicePublicKey())
	fingerprintText := hex.EncodeToString(fingerprint[:])

	authorize := &whalereinv1.PairingClaim{
		ClaimId:         request.GetClaimId(),
		TokenHash:       hashHex,
		Phase:           whalereinv1.PairingPhase_PAIRING_PHASE_AUTHORIZE,
		DeviceId:        request.GetDeviceId(),
		DevicePublicKey: claimResult.GetDevicePublicKey(),
		ExpiresAtUnixMs: now.Add(s.cfg.ClaimTTL).UnixMilli(),
	}
	authResult, err := s.forward(ctx, token.HostID, whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE, authorize, now)
	if err != nil {
		s.recordFailure(ctx, token.TokenHash, now)
		return nil, err
	}
	authorized, err := decodePairingResult(authResult)
	if err != nil || !authorized.GetAuthorized() {
		s.recordFailure(ctx, token.TokenHash, now)
		return nil, ErrHostRejected
	}
	commit := proto.Clone(authorize).(*whalereinv1.PairingClaim)
	commit.Phase = whalereinv1.PairingPhase_PAIRING_PHASE_COMMIT
	commit.TokenHash = ""
	body, _ := proto.Marshal(commit)
	notice := &whalereinv1.HostRequest{RequestId: "pair-commit-" + hex.EncodeToString(sha256SumCommit(token.HostID, request.DeviceId, request.ClaimId)), HostId: token.HostID, DeviceId: request.DeviceId, Kind: whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE, Body: body, ExpiresAtUnixMs: now.Add(s.cfg.ClaimTTL).UnixMilli(), ProtocolVersion: s.cfg.ProtocolVersion, TraceId: diagnostics.TraceID(ctx)}
	used, err := s.store.CommitPairing(ctx, token.TokenHash, token.HostID, request.GetDeviceId(), fingerprintText, authHash, now, notice)
	if err != nil {
		s.recordFailure(ctx, token.TokenHash, now)
		return nil, fmt.Errorf("%w: commit pairing", ErrInternal)
	}
	if !used {
		return nil, ErrTokenUsed
	}

	s.claimsMu.Lock()
	delete(s.claims, request.GetClaimId())
	s.claimsMu.Unlock()

	if state.hostFingerprint != "" {
		if err := s.store.UpdateHostMetadata(ctx, token.HostID, state.hostFingerprint, state.protocolVersion, now); err != nil {
			s.logger.Warn("host metadata update failed", "host", s.masker.Host(token.HostID), "error", err.Error())
		}
	}
	s.logger.Info("pairing grant committed",
		"host", s.masker.Host(token.HostID), "device", s.masker.Device(request.GetDeviceId()))
	response := &whalereinv1.PairingClaimResponse{
		ClaimId:         request.GetClaimId(),
		HostId:          token.HostID,
		Phase:           whalereinv1.PairingPhase_PAIRING_PHASE_FINISH,
		DeviceId:        request.GetDeviceId(),
		DevicePublicKey: claimResult.GetDevicePublicKey(),
		DeviceToken:     deviceToken,
		ExpiresAtUnixMs: token.ExpiresAt.UnixMilli(),
		ProtocolVersion: request.GetProtocolVersion(),
	}
	done := &completedClaim{tokenHash: sha256.Sum256([]byte(request.Token)), proof: sha256.Sum256(request.NoiseMessage), response: response, commit: notice, expiresAt: now.Add(s.cfg.ClaimTTL)}
	s.claimsMu.Lock()
	s.completed[request.ClaimId] = done
	s.claimsMu.Unlock()
	return s.completeClaim(ctx, done)

}

func sha256SumCommit(host, device, claim string) []byte {
	sum := sha256.Sum256([]byte(fmt.Sprintf("%d:%s%d:%s%d:%s", len(host), host, len(device), device, len(claim), claim)))
	return sum[:]
}

func (s *Service) completeClaim(ctx context.Context, done *completedClaim) (*whalereinv1.PairingClaimResponse, error) {
	if !done.ready {
		result, err := s.transport.PublishAndAwait(ctx, done.response.HostId, done.commit, s.cfg.ClaimTimeout)
		if err != nil {
			return nil, ErrHostUnavailable
		}
		committed, err := decodePairingResult(result)
		if err != nil || !committed.Authorized {
			return nil, ErrHostRejected
		}
		done.ready = true
		s.logger.Info("pairing finished", "host", s.masker.Host(done.response.HostId), "device", s.masker.Device(done.response.DeviceId), "trace_id", diagnostics.TraceID(ctx))
	}
	return proto.Clone(done.response).(*whalereinv1.PairingClaimResponse), nil
}

func (s *Service) forward(ctx context.Context, hostID string, kind whalereinv1.RequestKind, claim *whalereinv1.PairingClaim, now time.Time) (*whalereinv1.HostResult, error) {
	body, err := proto.Marshal(claim)
	if err != nil {
		return nil, fmt.Errorf("%w: marshal claim", ErrInternal)
	}
	requestID, err := randomID(s.random)
	if err != nil {
		return nil, fmt.Errorf("%w: generate request id", ErrInternal)
	}
	req := &whalereinv1.HostRequest{
		RequestId:       requestID,
		TraceId:         diagnostics.TraceID(ctx),
		HostId:          hostID,
		DeviceId:        claim.GetDeviceId(),
		Kind:            kind,
		ExpiresAtUnixMs: now.Add(s.cfg.ClaimTimeout).UnixMilli(),
		ProtocolVersion: s.cfg.ProtocolVersion,
		Body:            body,
	}
	result, err := s.transport.PublishAndAwait(ctx, hostID, req, s.cfg.ClaimTimeout)
	if err != nil {
		return nil, ErrHostUnavailable
	}
	if result.GetStatus() != whalereinv1.OnlineStatus_succeeded {
		if result.GetStatus() == whalereinv1.OnlineStatus_rejected {
			return nil, ErrHostRejected
		}
		return nil, ErrHostUnavailable
	}
	return result, nil
}

func decodePairingResult(result *whalereinv1.HostResult) (*whalereinv1.PairingClaimResult, error) {
	if result == nil {
		return nil, errors.New("empty result")
	}
	var claimResult whalereinv1.PairingClaimResult
	if err := proto.Unmarshal(result.GetResultBody(), &claimResult); err != nil {
		return nil, err
	}
	return &claimResult, nil
}

func (s *Service) recordFailure(ctx context.Context, tokenHash []byte, now time.Time) {
	if _, err := s.store.IncrementPairingFailure(ctx, tokenHash, now); err != nil {
		s.logger.Warn("pairing failure count update failed", "error", err.Error())
	}
}

func (s *Service) newDeviceToken() (string, []byte, error) {
	raw := make([]byte, 32)
	if _, err := io.ReadFull(s.random, raw); err != nil {
		return "", nil, fmt.Errorf("%w: generate device token", ErrInternal)
	}
	token := base64.RawURLEncoding.EncodeToString(raw)
	hash := sha256.Sum256([]byte(token))
	return token, hash[:], nil
}

func (s *Service) gcClaims(now time.Time) {
	s.claimsMu.Lock()
	defer s.claimsMu.Unlock()
	for id, state := range s.completed {
		if !now.Before(state.expiresAt) {
			delete(s.completed, id)
		}
	}
	for id, state := range s.claims {
		if !now.Before(state.expiresAt) {
			delete(s.claims, id)
		}
	}
}

type tokenLock struct {
	gate chan struct{}
	refs int
}

func (s *Service) lockToken(ctx context.Context, hashHex string) (func(), error) {
	s.locksMu.Lock()
	entry := s.locks[hashHex]
	if entry == nil {
		entry = &tokenLock{gate: make(chan struct{}, 1)}
		entry.gate <- struct{}{}
		s.locks[hashHex] = entry
	}
	entry.refs++
	s.locksMu.Unlock()
	release := func() {
		s.locksMu.Lock()
		entry.refs--
		if entry.refs == 0 {
			delete(s.locks, hashHex)
		}
		s.locksMu.Unlock()
	}
	if err := ctx.Err(); err != nil {
		release()
		return nil, err
	}
	select {
	case <-ctx.Done():
		release()
		return nil, ctx.Err()
	case <-entry.gate:
		return func() { entry.gate <- struct{}{}; release() }, nil
	}
}

func randomID(reader io.Reader) (string, error) {
	raw := make([]byte, 18)
	if _, err := io.ReadFull(reader, raw); err != nil {
		return "", err
	}
	return base64.RawURLEncoding.EncodeToString(raw), nil
}
