// Package pairing owns one-time invitations and first registration.
//
// HELLO and FINISH exchange opaque Noise messages with the Host. PostgreSQL
// owns their operation receipts; only transport correlation is transient. FINISH
// binds a client-generated credential hash to the verified device key. Grant,
// receipt and activation work commit together without storing plaintext tokens.
package pairing

import (
	"bytes"
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
	ErrUnauthorized                = errors.New("invalid pairing token")
	ErrTokenUsed                   = errors.New("pairing token already used")
	ErrTokenExpired                = errors.New("pairing token expired")
	ErrFailureBudget               = errors.New("pairing token failure budget exhausted")
	ErrHostOffline                 = errors.New("host offline")
	ErrHostRejected                = errors.New("host rejected pairing")
	ErrHostUnavailable             = errors.New("host unavailable")
	ErrClaimRequired               = errors.New("pairing HELLO is required before FINISH")
	ErrInvalidClaim                = errors.New("invalid pairing claim")
	ErrPairingFenced               = errors.New("pairing intent invalidated")
	ErrPairingCommittedUnavailable = errors.New("committed pairing is no longer available")
	ErrPairingCommittedUnconfirmed = errors.New("pairing committed but Host confirmation is unknown")
	ErrProtocolVersion             = errors.New("protocol version incompatible")
	ErrHostNotFound                = errors.New("host not found")
	ErrInternal                    = errors.New("pairing internal error")
)

// Store is the pairing persistence boundary.
type Store interface {
	PairingGenerations(ctx context.Context, tokenHash []byte, deviceID string) (int64, int64, error)
	SavePairingOperation(ctx context.Context, op store.PairingOperation) error
	PairingOperation(ctx context.Context, claimID string) (store.PairingOperation, error)
	PairingFenced(ctx context.Context, claimID, hostID, deviceID string) (bool, error)
	PairingOutcome(ctx context.Context, claimID, deviceID string) (store.PairingOutcome, error)
	ConfirmPairingHost(ctx context.Context, claimID, hostID, deviceID, requestID string, at time.Time) (bool, error)
	GetRequest(ctx context.Context, requestID string) (store.Request, error)
	GetHost(ctx context.Context, hostID string) (store.Host, error)
	CreatePairingToken(ctx context.Context, tokenHash []byte, hostID string, expiresAt time.Time, at time.Time) error
	PairingTokenByHash(ctx context.Context, tokenHash []byte) (store.PairingToken, error)
	IncrementPairingFailure(ctx context.Context, tokenHash []byte, at time.Time) (int, error)
	CommitPairing(ctx context.Context, tokenHash []byte, hostID, deviceID, fingerprint string, authHash []byte, at time.Time, notice *whalereinv1.HostRequest, operation *store.PairingOperation) (bool, error)
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

type completedClaim struct {
	response *whalereinv1.PairingClaimResponse
	commit   *whalereinv1.HostRequest
	ready    bool
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
	if request.Phase == whalereinv1.PairingPhase_PAIRING_PHASE_FINISH {
		raw, err := base64.RawURLEncoding.DecodeString(request.DeviceToken)
		if err != nil || len(raw) != 32 {
			return nil, ErrInvalidClaim
		}
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
	var deviceGeneration, hostGeneration int64
	if phase == whalereinv1.PairingPhase_PAIRING_PHASE_HELLO {
		var err error
		deviceGeneration, hostGeneration, err = s.store.PairingGenerations(ctx, hash[:], request.DeviceId)
		if errors.Is(err, store.ErrNotFound) {
			return nil, ErrUnauthorized
		}
		if err != nil {
			return nil, ErrInternal
		}
	}
	hashHex := hex.EncodeToString(hash[:])

	unlock, lockErr := s.lockToken(ctx, hashHex)
	if lockErr != nil {
		return nil, lockErr
	}
	defer unlock()
	now := s.now()

	op, opErr := s.store.PairingOperation(ctx, request.ClaimId)
	if opErr != nil && !errors.Is(opErr, store.ErrNotFound) {
		return nil, ErrInternal
	}
	if errors.Is(opErr, store.ErrNotFound) && phase == whalereinv1.PairingPhase_PAIRING_PHASE_FINISH {
		recovered, outcomeErr := s.committedOutcome(ctx, request, hash[:])
		if outcomeErr == nil {
			return recovered, nil
		}
		if !errors.Is(outcomeErr, store.ErrNotFound) {
			return nil, outcomeErr
		}
	}
	if opErr == nil {
		if !bytes.Equal(op.TokenHash, hash[:]) || op.DeviceID != request.DeviceId {
			return nil, ErrClaimRequired
		}
		if phase == whalereinv1.PairingPhase_PAIRING_PHASE_FINISH && len(op.FinishHash) == 0 {
			fenced, err := s.store.PairingFenced(ctx, op.ClaimID, op.HostID, op.DeviceID)
			if err != nil {
				return nil, ErrInternal
			}
			if fenced {
				return nil, ErrPairingFenced
			}
		}
		if !now.Before(op.ExpiresAt) {
			if phase == whalereinv1.PairingPhase_PAIRING_PHASE_FINISH && len(op.FinishHash) > 0 {
				recovered, outcomeErr := s.committedOutcome(ctx, request, hash[:])
				if outcomeErr == nil {
					return recovered, nil
				}
				if !errors.Is(outcomeErr, store.ErrNotFound) {
					return nil, outcomeErr
				}
			}
			return nil, ErrClaimRequired
		}
		if phase == whalereinv1.PairingPhase_PAIRING_PHASE_FINISH && len(op.FinishHash) > 0 {
			digest := claimDigest(request)
			if !bytes.Equal(op.FinishHash, digest) {
				return nil, ErrInvalidClaim
			}
			if !op.Active {
				return nil, ErrPairingCommittedUnavailable
			}
			// Never forward a legacy outbox until its persisted COMMIT binding
			// is proven. A Host may acknowledge AUTHORIZE without activation.
			outcome, outcomeErr := s.store.PairingOutcome(ctx, request.ClaimId, request.DeviceId)
			if errors.Is(outcomeErr, store.ErrNotFound) {
				return nil, ErrPairingCommittedUnconfirmed
			}
			if outcomeErr != nil {
				return nil, ErrInternal
			}
			if !bytes.Equal(outcome.TokenHash, hash[:]) || !bytes.Equal(outcome.FinishHash, digest) {
				return nil, ErrInvalidClaim
			}
			if !outcome.Active {
				return nil, ErrPairingCommittedUnavailable
			}
			if outcome.CommitBindingUnproven {
				return nil, ErrPairingCommittedUnconfirmed
			}
			if outcome.HostConfirmed {
				recovered, recoverErr := s.committedOutcome(ctx, request, hash[:])
				if recoverErr != nil {
					return nil, recoverErr
				}
				// The retained FINISH response carries the original invitation
				// expiry. Reuse it only when its identity matches the proof.
				var retained whalereinv1.PairingClaimResponse
				if proto.Unmarshal(op.FinishResponse, &retained) == nil && retained.DeviceToken == "" &&
					retained.ClaimId == recovered.ClaimId && retained.HostId == recovered.HostId && retained.DeviceId == recovered.DeviceId &&
					retained.Phase == whalereinv1.PairingPhase_PAIRING_PHASE_FINISH && retained.ProtocolVersion == recovered.ProtocolVersion &&
					bytes.Equal(retained.DevicePublicKey, recovered.DevicePublicKey) {
					recovered.ExpiresAtUnixMs = retained.ExpiresAtUnixMs
				}
				return recovered, nil
			}
			var response whalereinv1.PairingClaimResponse
			if proto.Unmarshal(op.FinishResponse, &response) != nil {
				return nil, ErrInternal
			}
			notice, err := s.store.GetRequest(ctx, op.CommitRequestID)
			if err != nil {
				if errors.Is(err, store.ErrNotFound) {
					return nil, ErrPairingCommittedUnconfirmed
				}
				return nil, ErrInternal
			}
			if len(notice.RequestBody) == 0 || !now.Before(notice.ExpiresAt) {
				return nil, ErrPairingCommittedUnconfirmed
			}
			var storedCommit whalereinv1.PairingClaimResult
			ready := notice.Status == whalereinv1.OnlineStatus_succeeded && proto.Unmarshal(notice.ResultBody, &storedCommit) == nil && storedCommit.Authorized && storedCommit.DeviceId == request.DeviceId
			done := &completedClaim{ready: ready, response: &response, commit: &whalereinv1.HostRequest{
				RequestId: notice.RequestID, HostId: notice.HostID, DeviceId: notice.DeviceID, Kind: notice.Kind,
				ProtocolVersion: notice.ProtocolVersion, Body: notice.RequestBody, ExpiresAtUnixMs: notice.ExpiresAt.UnixMilli()}}
			response.DeviceToken = request.DeviceToken
			return s.completeClaim(ctx, done)
		}
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

	if phase == whalereinv1.PairingPhase_PAIRING_PHASE_HELLO {
		if opErr == nil {
			proof := sha256.Sum256(request.NoiseMessage)
			if !bytes.Equal(proof[:], op.HelloHash) {
				return nil, ErrInvalidClaim
			}
			var reply whalereinv1.PairingClaimResponse
			if proto.Unmarshal(op.HelloResponse, &reply) != nil {
				return nil, ErrInternal
			}
			return &reply, nil
		}
		return s.hello(ctx, request, token, hashHex, now, deviceGeneration, hostGeneration)
	}
	if opErr != nil {
		return nil, ErrClaimRequired
	}
	return s.finish(ctx, request, token, op, now)
}

// committedOutcome recovers only an exact authenticated FINISH and separately
// requires a durable Host COMMIT acknowledgement for an active grant.
func (s *Service) committedOutcome(ctx context.Context, request *whalereinv1.PairingClaimRequest, tokenHash []byte) (*whalereinv1.PairingClaimResponse, error) {
	outcome, err := s.store.PairingOutcome(ctx, request.ClaimId, request.DeviceId)
	if err != nil {
		if errors.Is(err, store.ErrNotFound) {
			return nil, store.ErrNotFound
		}
		return nil, ErrInternal
	}
	if !bytes.Equal(outcome.TokenHash, tokenHash) || !bytes.Equal(outcome.FinishHash, claimDigest(request)) {
		return nil, ErrInvalidClaim
	}
	if !outcome.Active {
		return nil, ErrPairingCommittedUnavailable
	}
	if outcome.CommitBindingUnproven {
		return nil, ErrPairingCommittedUnconfirmed
	}
	if !outcome.HostConfirmed || len(outcome.DevicePublicKey) != 32 || outcome.ProtocolVersion != request.ProtocolVersion {
		return nil, ErrPairingCommittedUnconfirmed
	}
	return &whalereinv1.PairingClaimResponse{ClaimId: outcome.ClaimID, HostId: outcome.HostID, DeviceId: outcome.DeviceID,
		Phase: whalereinv1.PairingPhase_PAIRING_PHASE_FINISH, DevicePublicKey: outcome.DevicePublicKey,
		DeviceToken: request.DeviceToken, ProtocolVersion: outcome.ProtocolVersion}, nil
}

func (s *Service) hello(ctx context.Context, request *whalereinv1.PairingClaimRequest, token store.PairingToken, hashHex string, now time.Time, deviceGeneration, hostGeneration int64) (*whalereinv1.PairingClaimResponse, error) {

	if !s.presence.IsHostOnline(ctx, token.HostID) {
		s.recordFailure(ctx, token.TokenHash, now)
		return nil, ErrHostOffline
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
	observedAt := s.now()
	if !observedAt.Before(token.ExpiresAt) {
		return nil, ErrTokenExpired
	}
	if !observedAt.Before(now.Add(s.cfg.ClaimTTL)) {
		return nil, ErrClaimRequired
	}

	// HELLO is small enough to bootstrap an unknown peer using legacy framing.
	// Require verified capability after its reply, before creating a grant.
	if !s.transport.SupportsPairing(token.HostID) {
		return nil, ErrProtocolVersion
	}

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
	encoded, err := proto.Marshal(reply)
	if err != nil {
		return nil, ErrInternal
	}
	digest := sha256.Sum256(request.NoiseMessage)
	if err := s.store.SavePairingOperation(ctx, store.PairingOperation{ClaimID: request.ClaimId, TokenHash: token.TokenHash,
		HostID: token.HostID, DeviceID: request.DeviceId, DeviceGeneration: deviceGeneration, HostGeneration: hostGeneration, HelloHash: digest[:], HelloResponse: encoded, ExpiresAt: now.Add(s.cfg.ClaimTTL)}); err != nil {
		if errors.Is(err, store.ErrPairingFenced) {
			return nil, ErrPairingFenced
		}
		if errors.Is(err, store.ErrPairingInvitationExpired) {
			return nil, ErrTokenExpired
		}
		if errors.Is(err, store.ErrPairingIntentExpired) {
			return nil, ErrClaimRequired
		}
		return nil, ErrInternal
	}
	return proto.Clone(reply).(*whalereinv1.PairingClaimResponse), nil

}

func (s *Service) finish(ctx context.Context, request *whalereinv1.PairingClaimRequest, token store.PairingToken, operationState store.PairingOperation, now time.Time) (*whalereinv1.PairingClaimResponse, error) {
	var helloReply whalereinv1.PairingClaimResponse
	if proto.Unmarshal(operationState.HelloResponse, &helloReply) != nil {
		return nil, ErrInternal
	}
	hashHex := hex.EncodeToString(token.TokenHash)
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
	authHash := sha256.Sum256([]byte(request.DeviceToken))
	claim.DeviceAuthHash = authHash[:]
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

	if !bytes.Equal(claimResult.DeviceAuthHash, authHash[:]) {
		return nil, ErrHostRejected
	}
	deviceToken := request.DeviceToken
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
	// FINISH and AUTHORIZE both cross a Host transport boundary. Their reply
	// cannot extend an invitation, operation, or staged COMMIT deadline.
	commitAt := s.now()
	if !commitAt.Before(token.ExpiresAt) {
		return nil, ErrTokenExpired
	}
	if !commitAt.Before(operationState.ExpiresAt) || commitAt.UnixMilli() >= authorize.ExpiresAtUnixMs {
		return nil, ErrClaimRequired
	}
	commit := proto.Clone(authorize).(*whalereinv1.PairingClaim)
	commit.Phase = whalereinv1.PairingPhase_PAIRING_PHASE_COMMIT
	commit.TokenHash = ""
	body, _ := proto.Marshal(commit)
	notice := &whalereinv1.HostRequest{RequestId: "pair-commit-" + hex.EncodeToString(sha256SumCommit(token.HostID, request.DeviceId, request.ClaimId)), HostId: token.HostID, DeviceId: request.DeviceId, Kind: whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE, Body: body, ExpiresAtUnixMs: now.Add(s.cfg.ClaimTTL).UnixMilli(), ProtocolVersion: s.cfg.ProtocolVersion, TraceId: diagnostics.TraceID(ctx)}
	durableResponse := &whalereinv1.PairingClaimResponse{ClaimId: request.ClaimId, HostId: token.HostID,
		Phase: whalereinv1.PairingPhase_PAIRING_PHASE_FINISH, DeviceId: request.DeviceId,
		DevicePublicKey: claimResult.DevicePublicKey, ExpiresAtUnixMs: token.ExpiresAt.UnixMilli(), ProtocolVersion: request.ProtocolVersion}
	encodedResponse, _ := proto.Marshal(durableResponse)
	operation := &store.PairingOperation{ClaimID: request.ClaimId, FinishHash: claimDigest(request), FinishResponse: encodedResponse}
	used, err := s.store.CommitPairing(ctx, token.TokenHash, token.HostID, request.GetDeviceId(), fingerprintText, authHash[:], commitAt, notice, operation)
	if err != nil {
		if errors.Is(err, store.ErrPairingFenced) {
			return nil, ErrPairingFenced
		}
		if errors.Is(err, store.ErrPairingInvitationExpired) {
			return nil, ErrTokenExpired
		}
		if errors.Is(err, store.ErrPairingIntentExpired) {
			return nil, ErrClaimRequired
		}
		s.recordFailure(ctx, token.TokenHash, now)
		return nil, fmt.Errorf("%w: commit pairing", ErrInternal)
	}
	if !used {
		return nil, ErrTokenUsed
	}

	if helloReply.HostFingerprint != "" {
		if err := s.store.UpdateHostMetadata(ctx, token.HostID, helloReply.HostFingerprint, helloReply.ProtocolVersion, commitAt); err != nil {
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
	done := &completedClaim{response: response, commit: notice}

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
		if err != nil || result.GetStatus() != whalereinv1.OnlineStatus_succeeded || !committed.Authorized || committed.DeviceId != done.response.DeviceId || result.GetHostId() != done.response.HostId || result.GetDeviceId() != done.response.DeviceId || result.GetRequestId() != done.commit.RequestId {
			return nil, ErrHostRejected
		}
		done.ready = true
		s.logger.Info("pairing finished", "host", s.masker.Host(done.response.HostId), "device", s.masker.Device(done.response.DeviceId), "trace_id", diagnostics.TraceID(ctx))
	}
	confirmed, err := s.store.ConfirmPairingHost(ctx, done.response.ClaimId, done.response.HostId, done.response.DeviceId, done.commit.RequestId, s.now())
	if err != nil {
		return nil, ErrHostUnavailable
	}
	if !confirmed {
		return nil, ErrPairingCommittedUnavailable
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

func claimDigest(request *whalereinv1.PairingClaimRequest) []byte {
	encoded, _ := proto.MarshalOptions{Deterministic: true}.Marshal(request)
	hash := sha256.Sum256(encoded)
	return hash[:]
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
