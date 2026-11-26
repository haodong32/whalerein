// Package relay owns the authorization and lifecycle policy for the
// node-scoped temporary ciphertext directory.
//
// It sits between the public HTTP surface and access/internal/payload. The HTTP
// layer extracts headers and bodies; this package authenticates the Host
// signature or Device bearer, validates the exact scope, length, digest and
// expiry, and rechecks the active grant revisions before releasing bytes. A
// Device bearer is verified before any object read. A Host proof is verified
// against the key of the Host stored on the object, so the owner is read first,
// but a missing object never answers differently from a rejected proof for an
// unauthenticated caller. It never inspects ciphertext and never logs an object
// id, path or content.
package relay

import (
	"bytes"
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"math"
	"regexp"
	"strings"
	"time"

	"github.com/whalerein/whalerein/access/internal/auth"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/observation"
	"github.com/whalerein/whalerein/access/internal/payload"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// Epoch is the only relay contract this build serves. Every request must carry
// it or fails with ErrEpoch (protocol_version_incompatible).
const Epoch uint32 = 5

// Terminal relay errors. The HTTP layer maps each to the stable wire status and
// error code from proto/v1/WIRE.md.
var (
	ErrDisabled       = errors.New("relay directory is disabled")
	ErrInvalid        = errors.New("invalid relay request")
	ErrEpoch          = errors.New("unsupported relay epoch")
	ErrUnauthorized   = errors.New("relay request is unauthorized")
	ErrForbidden      = errors.New("relay scope is not authorized")
	ErrNotFound       = errors.New("relay object not found")
	ErrConflict       = errors.New("relay object conflict")
	ErrWrongOwner     = errors.New("relay object belongs to another owner")
	ErrExpired        = errors.New("relay object expired")
	ErrTooLarge       = errors.New("relay object exceeds the bound")
	ErrQuota          = errors.New("relay quota refused the request")
	ErrPressure       = errors.New("relay storage pressure")
	ErrDigestMismatch = errors.New("relay payload digest mismatch")
	ErrUnavailable    = errors.New("relay directory is unavailable")
)

// WrongOwnerError carries the requested owner node so the HTTP layer can name
// it in the WhaleRein-Owner-Node response header without parsing the message.
type WrongOwnerError struct {
	Requested string
}

func (e *WrongOwnerError) Error() string { return ErrWrongOwner.Error() }

// Is keeps errors.Is(err, ErrWrongOwner) working through the concrete type.
func (e *WrongOwnerError) Is(target error) bool { return target == ErrWrongOwner }

var (
	objectIDPattern = regexp.MustCompile(`^[0-9a-f]{32}$`)
	// hostIDPattern and deviceIDPattern follow the shared identifier contract:
	// printable ASCII without the canonical-text separator.
	hostIDPattern   = regexp.MustCompile(`^[\x20-\x7b\x7d-\x7e]{1,200}$`)
	deviceIDPattern = regexp.MustCompile(`^[\x20-\x7b\x7d-\x7e]{1,200}$`)
)

// Config is the relay's view of the owning node and its transfer bounds. The
// payload store keeps the authoritative quota; these values only shape the
// reservation reply and the request expiry bound.
type Config struct {
	Enabled          bool
	NodeID           string
	OwnerID          string
	UploadPathPrefix string
	Retention        time.Duration
	UploadTimeout    time.Duration
	ReadTimeout      time.Duration
}

func (c Config) owner() string {
	if c.OwnerID != "" {
		return c.OwnerID
	}
	return c.NodeID
}

func (c Config) uploadPrefix() string {
	prefix := strings.TrimSpace(c.UploadPathPrefix)
	if prefix == "" {
		return "/v1/relay/objects"
	}
	return "/" + strings.Trim(prefix, "/")
}

// AuthService is the authority boundary this package consumes. It is the same
// auth service that serves pairing and revocation; relay proofs use a separate
// canonical-text path in that service.
type AuthService interface {
	AuthenticateDeviceToken(ctx context.Context, token string) (auth.Identity, error)
	AuthorizeHost(ctx context.Context, identity auth.Identity, hostID string) error
	AuthenticateRelayRequest(ctx context.Context, purpose string, fields auth.RelayFields, signature []byte) error
}

// Store supplies the durable authority and metadata used by the relay.
type Store interface {
	GetRequest(context.Context, string) (store.Request, error)
	GetPayload(ctx context.Context, objectID string) (store.PayloadObject, error)
	AdmissionGrantFor(ctx context.Context, hostID, deviceID string) (store.AdmissionGrant, error)
}

// Payloads is the node-local ciphertext service. access/internal/payload owns
// the actual staging, verification and file publication.
type Payloads interface {
	Reserve(ctx context.Context, input payload.ReserveInput) (payload.Object, error)
	Write(ctx context.Context, ref payload.Ref, body io.Reader) error
	Finish(ctx context.Context, ref payload.Ref) (payload.Object, error)
	Open(ctx context.Context, ref payload.Ref) (io.ReadCloser, payload.Object, error)
	Cancel(ctx context.Context, ref payload.Ref) error
}

// QueryScopes authorizes a ciphertext reservation against one live transient
// query waiter. It is optional: a disabled or command-only directory never
// consults it.
type QueryScopes interface {
	AuthorizeObjectReserve(ctx context.Context, hostID, deviceID, channelID, requestID, connectionRef string, enrollment uint64, expiresAtUnixMs int64) (string, error)
}

// Dependencies wires the relay service.
type Dependencies struct {
	Auth        AuthService
	Store       Store
	Payloads    Payloads
	QueryScopes QueryScopes
	Config      Config
	Now         func() time.Time
	Metrics     *diagnostics.Metrics
	Observation *observation.Sink
}

// Service implements the scoped ciphertext directory policy.
type Service struct {
	auth        AuthService
	store       Store
	payloads    Payloads
	queryScopes QueryScopes
	config      Config
	now         func() time.Time
	metrics     *diagnostics.Metrics
	observation *observation.Sink
}

// New validates the seams. A disabled directory still returns a service so the
// HTTP routes exist and answer unsupported_capability instead of 404.
func New(deps Dependencies) (*Service, error) {
	if deps.Now == nil {
		deps.Now = func() time.Time { return time.Now().UTC() }
	}
	if deps.Metrics == nil {
		deps.Metrics = diagnostics.NewMetrics()
	}
	if deps.Config.Enabled {
		if deps.Auth == nil || deps.Store == nil || deps.Payloads == nil {
			return nil, errors.New("relay: auth, store and payloads are required when enabled")
		}
		if strings.TrimSpace(deps.Config.owner()) == "" {
			return nil, errors.New("relay: an owner node id is required when enabled")
		}
		if deps.Config.Retention <= 0 {
			return nil, errors.New("relay: a positive retention is required when enabled")
		}
	}
	return &Service{
		auth:        deps.Auth,
		store:       deps.Store,
		payloads:    deps.Payloads,
		queryScopes: deps.QueryScopes,
		config:      deps.Config,
		now:         deps.Now,
		metrics:     deps.Metrics,
		observation: deps.Observation,
	}, nil
}

// Enabled reports whether the directory may reserve or release objects.
func (s *Service) Enabled() bool { return s != nil && s.config.Enabled }

// SetQueryScopes installs the transient query waiter authority. It is called
// once during wiring, after the query service exists.
func (s *Service) SetQueryScopes(scopes QueryScopes) {
	if s != nil {
		s.queryScopes = scopes
	}
}

// ReserveRequest is one decoded reservation body plus the transport
// credentials. Host callers sign the body; Device callers send a bearer token.
type ReserveRequest struct {
	RelayVersion    uint32
	HostID          string
	DeviceID        string
	SessionID       string
	ChannelID       string
	Purpose         whalereinv1.PayloadPurpose
	PartID          string
	CiphertextBytes uint64
	SHA256          []byte
	ExpiresAtUnixMs int64
	SignedAtUnixMs  int64
	Signature       []byte
	BearerToken     string
	OwnerNode       string
	// Query-lane binding. RequestID, ConnectionRef and EnrollmentGeneration
	// are set only for a result reserved under a live transient query waiter.
	// Durable command objects and attachment parts keep them empty/zero.
	RequestID            string
	ConnectionRef        string
	EnrollmentGeneration uint64
}

// Reservation is the accepted reservation returned to the caller.
type Reservation struct {
	ObjectID        string
	OwnerNodeID     string
	UploadPath      string
	MaxBytes        uint64
	ExpiresAtUnixMs int64
	UploadTimeoutMS uint32
}

// ObjectRequest is the shared path-header envelope for upload, finish, fetch
// and delete. RelayVersion comes from WhaleRein-Relay-Epoch.
type ObjectRequest struct {
	ObjectID       string
	RelayVersion   uint32
	BearerToken    string
	OwnerNode      string
	SignedAtUnixMs int64
	Signature      []byte
}

// UploadRequest adds the declared transport body length.
type UploadRequest struct {
	ObjectRequest
	ContentLength    int64
	HasContentLength bool
}

// FinishRequest is one decoded finish body.
type FinishRequest struct {
	ObjectRequest
	CiphertextBytes uint64
	SHA256          []byte
}

// UploadTarget is the authorized destination for one exact-length upload. The
// HTTP layer caps the body at Length+1 before streaming it.
type UploadTarget struct {
	Ref    payload.Ref
	Length int64
	// ObjectID and RequestID carry the observation join identity authorized by
	// PrepareUpload. They confer no additional authority.
	ObjectID  string
	RequestID string
	HostID    string
	DeviceID  string
}

// FetchResult is one authorized reader plus the metadata required for the
// response headers. The caller closes Body.
type FetchResult struct {
	Body            io.ReadCloser
	Length          int64
	Digest          []byte
	ExpiresAtUnixMs int64
}

// Object is the published ciphertext metadata.
type Object struct {
	ObjectID        string
	OwnerNodeID     string
	Purpose         whalereinv1.PayloadPurpose
	CiphertextBytes uint64
	SHA256          []byte
	ExpiresAtUnixMs int64
}

// DeleteResult reports whether this call released a live object.
type DeleteResult struct {
	Cancelled bool
}

// Reserve validates, authorizes and admits one exact ciphertext object.
func (s *Service) Reserve(ctx context.Context, req ReserveRequest) (reservation Reservation, err error) {
	started := time.Now()
	defer func() {
		if err != nil {
			// A failure before reservation has no object identity; the empty
			// identity token is a stable sentinel, never a real object id.
			if s.observation != nil {
				s.observeRelay(relayScope(req.HostID, req.DeviceID), relayRequestID(req.RequestID, ""), "", observation.RelayReserve, observation.OutcomeError, 0)
			}
			if s.metrics != nil {
				s.metrics.ObserveAccessStage("relay_stage", time.Since(started), string(observation.OutcomeError))
			}
			return
		}
		requestID := relayRequestID(req.RequestID, reservation.ObjectID)
		if s.observation != nil {
			s.observeRelay(relayScope(req.HostID, req.DeviceID), requestID, reservation.ObjectID, observation.RelayReserve, observation.OutcomeOK, int(reservation.MaxBytes))
		}
		if s.metrics != nil {
			s.metrics.ObserveAccessStage("relay_stage", time.Since(started), string(observation.OutcomeOK))
			s.metrics.ObserveAccessStageBytes("relay_stage", int64(reservation.MaxBytes))
		}
	}()
	if !s.Enabled() {
		return Reservation{}, ErrDisabled
	}
	if req.RelayVersion != Epoch {
		return Reservation{}, ErrEpoch
	}
	if err := s.checkOwner(req.OwnerNode); err != nil {
		return Reservation{}, err
	}
	if err := s.validateReserve(req); err != nil {
		return Reservation{}, err
	}
	grant, err := s.authorizeReserve(ctx, req)
	if err != nil {
		return Reservation{}, err
	}
	sessionID, err := s.reservationSession(ctx, req, grant)
	if err != nil {
		return Reservation{}, err
	}
	objectID, err := newObjectID()
	if err != nil {
		return Reservation{}, err
	}
	// Result objects have an admitted parent. A pre-admission command body or
	// attachment part is identified by its own opaque message id.
	messageID := objectID
	var enrollment *int64
	if req.RequestID != "" {
		messageID = ""
		enrollment = int64Ptr(grant.EnrollmentGeneration)
	}
	object, err := s.payloads.Reserve(ctx, payload.ReserveInput{
		ObjectID:                   objectID,
		HostID:                     req.HostID,
		DeviceID:                   req.DeviceID,
		AppSession:                 sessionID,
		ChannelID:                  req.ChannelID,
		RequestID:                  req.RequestID,
		MessageID:                  messageID,
		Purpose:                    purposeName(req.Purpose),
		PartID:                     req.PartID,
		Length:                     int64(req.CiphertextBytes),
		Digest:                     append([]byte(nil), req.SHA256...),
		ParentDeadline:             time.UnixMilli(req.ExpiresAtUnixMs).UTC(),
		GrantGeneration:            int64Ptr(grant.GrantGeneration),
		RevocationGeneration:       int64Ptr(grant.RevocationGeneration),
		DeviceRevocationGeneration: int64Ptr(grant.DeviceRevocationGeneration),
		EnrollmentGeneration:       enrollment,
		ConnectionRef:              req.ConnectionRef,
	})
	if err != nil {
		return Reservation{}, translate(err)
	}
	timeout := s.config.UploadTimeout
	if timeout <= 0 {
		timeout = time.Minute
	}
	return Reservation{
		ObjectID:        object.ObjectID,
		OwnerNodeID:     object.OwnerID,
		UploadPath:      s.config.uploadPrefix() + "/" + object.ObjectID,
		MaxBytes:        uint64(object.Length),
		ExpiresAtUnixMs: object.ExpiresAt.UnixMilli(),
		UploadTimeoutMS: uint32(timeout / time.Millisecond),
	}, nil
}

func (s *Service) validateReserve(req ReserveRequest) error {
	switch {
	case !hostIDPattern.MatchString(req.HostID):
		return fmt.Errorf("%w: host id is invalid", ErrInvalid)
	case !deviceIDPattern.MatchString(req.DeviceID):
		return fmt.Errorf("%w: device id is invalid", ErrInvalid)
	case req.ChannelID == "":
		return fmt.Errorf("%w: channel id is required", ErrInvalid)
	case req.CiphertextBytes == 0 || req.CiphertextBytes > math.MaxInt64:
		return fmt.Errorf("%w: ciphertext length is invalid", ErrInvalid)
	case len(req.SHA256) != sha256.Size:
		return fmt.Errorf("%w: digest must be SHA-256", ErrInvalid)
	case req.ExpiresAtUnixMs <= 0:
		return fmt.Errorf("%w: expiry is required", ErrInvalid)
	}
	if err := validUTF8Component("session id", req.SessionID, 512); err != nil {
		return err
	}
	if err := validUTF8Component("channel id", req.ChannelID, 128); err != nil {
		return err
	}
	if err := validUTF8Component("part id", req.PartID, 512); err != nil {
		return err
	}
	if req.Purpose < whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_REQUEST ||
		req.Purpose > whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_HISTORY_PAGE {
		return fmt.Errorf("%w: unsupported payload purpose", ErrInvalid)
	}
	if req.Purpose == whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_ATTACHMENT_PART && req.PartID == "" {
		return fmt.Errorf("%w: an attachment part requires a part id", ErrInvalid)
	}
	now := s.now()
	if req.ExpiresAtUnixMs <= now.UnixMilli() {
		return fmt.Errorf("%w: expiry has passed", ErrInvalid)
	}
	if s.config.Retention > 0 && req.ExpiresAtUnixMs > now.Add(s.config.Retention).UnixMilli() {
		return fmt.Errorf("%w: expiry exceeds the node retention", ErrInvalid)
	}
	return nil
}

// authorizeReserve authenticates the caller and returns the active grant whose
// revisions the reservation binds.
func (s *Service) authorizeReserve(ctx context.Context, req ReserveRequest) (store.AdmissionGrant, error) {
	if strings.TrimSpace(req.BearerToken) != "" {
		identity, err := s.auth.AuthenticateDeviceToken(ctx, req.BearerToken)
		if err != nil {
			return store.AdmissionGrant{}, translateAuth(err)
		}
		if identity.DeviceID != req.DeviceID {
			return store.AdmissionGrant{}, ErrForbidden
		}
		if identity.SessionID != req.SessionID {
			return store.AdmissionGrant{}, ErrForbidden
		}
		if err := s.auth.AuthorizeHost(ctx, identity, req.HostID); err != nil {
			return store.AdmissionGrant{}, translateAuth(err)
		}
	} else {
		if len(req.Signature) == 0 {
			return store.AdmissionGrant{}, ErrUnauthorized
		}
		if err := s.auth.AuthenticateRelayRequest(ctx, auth.RelayAuthReserve, auth.RelayFields{
			RelayVersion:    req.RelayVersion,
			HostID:          req.HostID,
			DeviceID:        req.DeviceID,
			SessionID:       req.SessionID,
			ChannelID:       req.ChannelID,
			Purpose:         req.Purpose,
			PartID:          req.PartID,
			CiphertextBytes: req.CiphertextBytes,
			SHA256:          req.SHA256,
			ExpiresAtUnixMs: req.ExpiresAtUnixMs,
			SignedAtUnixMs:  req.SignedAtUnixMs,
		}, req.Signature); err != nil {
			return store.AdmissionGrant{}, translateAuth(err)
		}
	}
	return s.activeGrant(ctx, req.HostID, req.DeviceID)
}

// reservationSession derives the stored login-session scope from the verified
// Device identity or the original Broker connection of a Host result.
func (s *Service) reservationSession(ctx context.Context, req ReserveRequest, grant store.AdmissionGrant) (string, error) {
	if req.RequestID == "" {
		if req.ConnectionRef != "" || req.EnrollmentGeneration != 0 {
			return "", fmt.Errorf("%w: connection scope requires a request id", ErrInvalid)
		}
		return req.SessionID, nil
	}
	if req.Purpose != whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_RESULT &&
		req.Purpose != whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_HISTORY_PAGE {
		return "", fmt.Errorf("%w: query scope is only valid for result ciphertext", ErrInvalid)
	}
	if err := validUTF8Component("request id", req.RequestID, 512); err != nil {
		return "", err
	}
	if err := validUTF8Component("connection ref", req.ConnectionRef, 128); err != nil {
		return "", err
	}
	if req.ConnectionRef == "" {
		// Durable result objects belong to the command admitted by Access.
		row, err := s.store.GetRequest(ctx, req.RequestID)
		if err != nil {
			if errors.Is(err, store.ErrNotFound) {
				return "", ErrForbidden
			}
			return "", err
		}
		if row.Kind != whalereinv1.RequestKind_REQUEST_KIND_COMMAND || row.HostID != req.HostID ||
			row.DeviceID != req.DeviceID || row.ChannelID != req.ChannelID || row.Stamp == nil ||
			row.Stamp.GrantGeneration != grant.GrantGeneration || row.Stamp.RevocationGeneration != grant.RevocationGeneration ||
			row.Stamp.DeviceRevocationGeneration != grant.DeviceRevocationGeneration ||
			req.ExpiresAtUnixMs > row.ExpiresAt.UnixMilli() || (req.SessionID != "" && req.SessionID != row.DeviceSessionID) {
			return "", ErrForbidden
		}
		return row.DeviceSessionID, nil
	}
	if !validConnectionRef(req.ConnectionRef) {
		return "", fmt.Errorf("%w: connection ref is invalid", ErrInvalid)
	}
	if req.EnrollmentGeneration != grant.EnrollmentGeneration {
		return "", ErrForbidden
	}
	if s.queryScopes == nil {
		return "", ErrForbidden
	}
	sessionID, err := s.queryScopes.AuthorizeObjectReserve(ctx, req.HostID, req.DeviceID, req.ChannelID,
		req.RequestID, req.ConnectionRef, req.EnrollmentGeneration, req.ExpiresAtUnixMs)
	if err != nil || (req.SessionID != "" && req.SessionID != sessionID) {
		return "", ErrForbidden
	}
	return sessionID, nil
}

// validConnectionRef checks the Broker-issued <epoch>:<connection> shape.
func validConnectionRef(value string) bool {
	epoch, instance, ok := strings.Cut(value, ":")
	if !ok || epoch == "" || instance == "" {
		return false
	}
	for _, r := range epoch {
		if (r < '0' || r > '9') && (r < 'a' || r > 'f') {
			return false
		}
	}
	for _, r := range instance {
		if r < '0' || r > '9' {
			return false
		}
	}
	return true
}

// checkResultParent repeats the parent authority check using the binding
// stored at reservation. A missing waiter or command cannot publish bytes.
func (s *Service) checkResultParent(ctx context.Context, object store.PayloadObject) error {
	if object.RequestID == "" && object.ConnectionRef == "" {
		return nil
	}
	if object.RequestID == "" || object.EnrollmentGeneration == nil || *object.EnrollmentGeneration < 0 ||
		object.GrantGeneration == nil || object.RevocationGeneration == nil || object.DeviceRevocationGeneration == nil {
		return ErrForbidden
	}
	binding := ReserveRequest{
		HostID: object.HostID, DeviceID: object.DeviceID, SessionID: object.AppSession,
		ChannelID: object.ChannelID, RequestID: object.RequestID, ConnectionRef: object.ConnectionRef,
		Purpose:              whalereinv1.PayloadPurpose(whalereinv1.PayloadPurpose_value[object.Purpose]),
		EnrollmentGeneration: uint64(*object.EnrollmentGeneration), ExpiresAtUnixMs: object.ExpiresAt.UnixMilli(),
	}
	grant := store.AdmissionGrant{
		GrantGeneration: uint64(*object.GrantGeneration), RevocationGeneration: uint64(*object.RevocationGeneration),
		DeviceRevocationGeneration: uint64(*object.DeviceRevocationGeneration), EnrollmentGeneration: binding.EnrollmentGeneration,
	}
	session, err := s.reservationSession(ctx, binding, grant)
	if err != nil {
		return err
	}
	if session != object.AppSession {
		return ErrForbidden
	}
	return nil
}

// PrepareUpload authorizes one exact-length upload before the HTTP layer reads
// a single body byte. The returned target carries the reserved scope.
func (s *Service) PrepareUpload(ctx context.Context, req UploadRequest) (UploadTarget, error) {
	object, err := s.loadObject(ctx, req.ObjectRequest, func(object store.PayloadObject) error {
		return s.verifyHost(ctx, auth.RelayAuthUpload, req.ObjectRequest, auth.RelayFields{
			RelayVersion:    req.RelayVersion,
			HostID:          object.HostID,
			ObjectID:        object.ObjectID,
			CiphertextBytes: uint64(object.Length),
			SHA256:          object.Digest,
			SignedAtUnixMs:  req.SignedAtUnixMs,
		})
	}, true)
	if err != nil {
		return UploadTarget{}, err
	}
	if !req.HasContentLength {
		return UploadTarget{}, fmt.Errorf("%w: Content-Length is required", ErrInvalid)
	}
	if req.ContentLength != object.Length {
		return UploadTarget{}, fmt.Errorf("%w: Content-Length does not match the reservation", ErrConflict)
	}
	return UploadTarget{Ref: refFor(object), Length: object.Length, ObjectID: object.ObjectID, RequestID: object.RequestID, HostID: object.HostID, DeviceID: object.DeviceID}, nil
}

// Upload streams the caller's body through the payload service. The caller
// must already have capped it at the reserved length plus one.
func (s *Service) Upload(ctx context.Context, target UploadTarget, body io.Reader) (err error) {
	started := time.Now()
	read := &countingReader{reader: body}
	requestID := relayRequestID(target.RequestID, target.ObjectID)
	defer func() {
		outcome := relayOutcome(err)
		s.observeRelay(relayScope(target.HostID, target.DeviceID), requestID, target.ObjectID, observation.RelayUpload, outcome, read.count)
		if s.metrics != nil {
			s.metrics.ObserveAccessStage("relay_upload", time.Since(started), string(outcome))
			s.metrics.ObserveAccessStageBytes("relay_upload", int64(read.count))
		}
	}()
	if !s.Enabled() {
		return ErrDisabled
	}
	if body == nil || target.Length <= 0 {
		return fmt.Errorf("%w: upload target is incomplete", ErrInvalid)
	}
	return translate(s.payloads.Write(ctx, target.Ref, read))
}

// Finish verifies the declared length and digest and publishes the staged
// object.
func (s *Service) Finish(ctx context.Context, req FinishRequest) (Object, error) {
	started := time.Now()
	object, err := s.loadObject(ctx, req.ObjectRequest, func(object store.PayloadObject) error {
		return s.verifyHost(ctx, auth.RelayAuthFinish, req.ObjectRequest, auth.RelayFields{
			RelayVersion:    req.RelayVersion,
			HostID:          object.HostID,
			ObjectID:        object.ObjectID,
			CiphertextBytes: req.CiphertextBytes,
			SHA256:          req.SHA256,
			SignedAtUnixMs:  req.SignedAtUnixMs,
		})
	}, true)
	if err != nil {
		s.observeRelay("", relayRequestID("", req.ObjectID), req.ObjectID, observation.RelayFinish, observation.OutcomeError, 0)
		if s.metrics != nil {
			s.metrics.ObserveAccessStage("relay_finish", time.Since(started), string(observation.OutcomeError))
		}
		return Object{}, err
	}
	requestID := relayRequestID(object.RequestID, object.ObjectID)
	if err := s.checkResultParent(ctx, object); err != nil {
		s.observeRelay(relayScope(object.HostID, object.DeviceID), requestID, object.ObjectID, observation.RelayFinish, observation.OutcomeError, 0)
		if s.metrics != nil {
			s.metrics.ObserveAccessStage("relay_finish", time.Since(started), string(observation.OutcomeError))
		}
		return Object{}, err
	}
	if req.CiphertextBytes != uint64(object.Length) {
		s.observeRelay(relayScope(object.HostID, object.DeviceID), requestID, object.ObjectID, observation.RelayFinish, observation.OutcomeError, 0)
		if s.metrics != nil {
			s.metrics.ObserveAccessStage("relay_finish", time.Since(started), string(observation.OutcomeError))
		}
		return Object{}, fmt.Errorf("%w: finish length does not match the reservation", ErrConflict)
	}
	if !bytes.Equal(req.SHA256, object.Digest) {
		s.observeRelay(relayScope(object.HostID, object.DeviceID), requestID, object.ObjectID, observation.RelayFinish, observation.OutcomeError, 0)
		if s.metrics != nil {
			s.metrics.ObserveAccessStage("relay_finish", time.Since(started), string(observation.OutcomeError))
		}
		return Object{}, ErrDigestMismatch
	}
	// finish records the declared, authorized length before verification; the
	// verify stage records the digest-verified logical bytes exactly once.
	s.observeRelay(relayScope(object.HostID, object.DeviceID), requestID, object.ObjectID, observation.RelayFinish, observation.OutcomeOK, int(object.Length))
	if s.metrics != nil {
		s.metrics.ObserveAccessStage("relay_finish", time.Since(started), string(observation.OutcomeOK))
	}
	verifyStarted := time.Now()
	finished, err := s.payloads.Finish(ctx, refFor(object))
	if err != nil {
		s.observeRelay(relayScope(object.HostID, object.DeviceID), requestID, object.ObjectID, observation.RelayVerify, observation.OutcomeError, 0)
		if s.metrics != nil {
			s.metrics.ObserveAccessStage("relay_verify", time.Since(verifyStarted), string(observation.OutcomeError))
		}
		return Object{}, translate(err)
	}
	s.observeRelay(relayScope(object.HostID, object.DeviceID), requestID, object.ObjectID, observation.RelayVerify, observation.OutcomeOK, int(finished.Length))
	if s.metrics != nil {
		s.metrics.ObserveAccessStage("relay_verify", time.Since(verifyStarted), string(observation.OutcomeOK))
		s.metrics.ObserveAccessStageBytes("relay_verify", int64(finished.Length))
	}
	return objectResult(finished), nil
}

// Fetch rechecks authority and opens the exact ciphertext. The caller streams
// and closes the returned reader.
func (s *Service) Fetch(ctx context.Context, req ObjectRequest) (FetchResult, error) {
	started := time.Now()
	object, err := s.loadObject(ctx, req, func(object store.PayloadObject) error {
		return s.verifyHost(ctx, auth.RelayAuthObject, req, auth.RelayFields{
			RelayVersion:   req.RelayVersion,
			HostID:         object.HostID,
			ObjectID:       object.ObjectID,
			SignedAtUnixMs: req.SignedAtUnixMs,
		})
	}, true)
	if err != nil {
		s.observeRelay("", relayRequestID("", req.ObjectID), req.ObjectID, observation.RelayFetch, observation.OutcomeError, 0)
		if s.metrics != nil {
			s.metrics.ObserveAccessStage("relay_fetch", time.Since(started), string(observation.OutcomeError))
		}
		return FetchResult{}, err
	}
	requestID := relayRequestID(object.RequestID, object.ObjectID)
	// A query-scoped object is releasable only after its reply was issued.
	if object.RequestID != "" && object.ConnectionRef != "" && object.IssuedAt == nil {
		s.observeRelay(relayScope(object.HostID, object.DeviceID), requestID, object.ObjectID, observation.RelayFetch, observation.OutcomeError, 0)
		if s.metrics != nil {
			s.metrics.ObserveAccessStage("relay_fetch", time.Since(started), string(observation.OutcomeError))
		}
		return FetchResult{}, ErrForbidden
	}
	reader, opened, err := s.payloads.Open(ctx, refFor(object))
	if err != nil {
		s.observeRelay(relayScope(object.HostID, object.DeviceID), requestID, object.ObjectID, observation.RelayFetch, observation.OutcomeError, 0)
		if s.metrics != nil {
			s.metrics.ObserveAccessStage("relay_fetch", time.Since(started), string(observation.OutcomeError))
		}
		return FetchResult{}, translate(err)
	}
	s.observeRelay(relayScope(object.HostID, object.DeviceID), requestID, object.ObjectID, observation.RelayFetch, observation.OutcomeOK, int(opened.Length))
	if s.metrics != nil {
		s.metrics.ObserveAccessStage("relay_fetch", time.Since(started), string(observation.OutcomeOK))
		s.metrics.ObserveAccessStageBytes("relay_fetch", int64(opened.Length))
	}
	return FetchResult{
		Body:            reader,
		Length:          opened.Length,
		Digest:          append([]byte(nil), opened.Digest...),
		ExpiresAtUnixMs: opened.ExpiresAt.UnixMilli(),
	}, nil
}

// Delete cancels one object. It is idempotent for the same authorized scope and
// deliberately allowed after grant revocation so a caller can always clean up
// its own ciphertext.
func (s *Service) Delete(ctx context.Context, req ObjectRequest) (DeleteResult, error) {
	object, err := s.loadObject(ctx, req, func(object store.PayloadObject) error {
		return s.verifyHost(ctx, auth.RelayAuthObject, req, auth.RelayFields{
			RelayVersion:   req.RelayVersion,
			HostID:         object.HostID,
			ObjectID:       object.ObjectID,
			SignedAtUnixMs: req.SignedAtUnixMs,
		})
	}, false)
	if err != nil {
		return DeleteResult{}, err
	}
	live := (object.State == store.PayloadReserved || object.State == store.PayloadReady) &&
		object.ExpiresAt.After(s.now())
	if err := s.payloads.Cancel(ctx, refFor(object)); err != nil {
		translated := translate(err)
		// A concurrent cleanup can remove the row between the metadata read
		// and the cancel. The exact scope is already terminal, so the repeated
		// call is an idempotent no-op.
		if errors.Is(translated, ErrNotFound) {
			return DeleteResult{Cancelled: false}, nil
		}
		return DeleteResult{}, translated
	}
	return DeleteResult{Cancelled: live}, nil
}

// loadObject performs the checks shared by every object route: epoch, owner,
// object id, authentication, expiry and (optionally) the bound grant
// revisions. The verify callback runs only for Host-authority callers and
// builds the operation-specific canonical text.
func (s *Service) loadObject(ctx context.Context, req ObjectRequest, verify func(store.PayloadObject) error, requireGrant bool) (store.PayloadObject, error) {
	if !s.Enabled() {
		return store.PayloadObject{}, ErrDisabled
	}
	if req.RelayVersion != Epoch {
		return store.PayloadObject{}, ErrEpoch
	}
	if err := s.checkOwner(req.OwnerNode); err != nil {
		return store.PayloadObject{}, err
	}
	if !objectIDPattern.MatchString(req.ObjectID) {
		return store.PayloadObject{}, fmt.Errorf("%w: object id must be 32 lowercase hex digits", ErrInvalid)
	}
	useDevice := strings.TrimSpace(req.BearerToken) != ""
	if !useDevice && len(req.Signature) == 0 {
		return store.PayloadObject{}, ErrUnauthorized
	}
	// Authenticate a Device bearer before touching the store: an invalid
	// credential must answer the same way whether or not the object exists.
	var identity auth.Identity
	if useDevice {
		resolved, err := s.auth.AuthenticateDeviceToken(ctx, req.BearerToken)
		if err != nil {
			return store.PayloadObject{}, translateAuth(err)
		}
		identity = resolved
	}
	object, err := s.store.GetPayload(ctx, req.ObjectID)
	if err != nil {
		if errors.Is(err, store.ErrNotFound) {
			if useDevice {
				return store.PayloadObject{}, ErrNotFound
			}
			// A Host proof is verified against the key of the Host stored on
			// the object, so a missing object cannot be authenticated without
			// first learning that it exists. Answer every unauthenticated Host
			// request the same way to keep object existence out of the status.
			return store.PayloadObject{}, ErrUnauthorized
		}
		return store.PayloadObject{}, err
	}
	if useDevice {
		if err := s.checkDeviceScope(ctx, identity, object); err != nil {
			return store.PayloadObject{}, err
		}
	} else {
		if err := verify(object); err != nil {
			return store.PayloadObject{}, err
		}
	}
	if !object.ExpiresAt.After(s.now()) {
		return store.PayloadObject{}, ErrExpired
	}
	if requireGrant {
		if err := s.checkGrant(ctx, object); err != nil {
			return store.PayloadObject{}, err
		}
	}
	return object, nil
}

func (s *Service) verifyHost(ctx context.Context, purpose string, req ObjectRequest, fields auth.RelayFields) error {
	if len(req.Signature) == 0 {
		return ErrUnauthorized
	}
	if err := s.auth.AuthenticateRelayRequest(ctx, purpose, fields, req.Signature); err != nil {
		return translateAuth(err)
	}
	return nil
}

// checkDeviceScope binds a Device caller to the stored reservation. A Device
// can never read another Device's object, and a session-bound object requires
// the same managed session.
func (s *Service) checkDeviceScope(ctx context.Context, identity auth.Identity, object store.PayloadObject) error {
	if object.DeviceID == "" || identity.DeviceID != object.DeviceID {
		return ErrForbidden
	}
	if (object.RequestID != "" || object.AppSession != "") && identity.SessionID != object.AppSession {
		return ErrForbidden
	}
	if err := s.auth.AuthorizeHost(ctx, identity, object.HostID); err != nil {
		return translateAuth(err)
	}
	return nil
}

// activeGrant reads the authoritative grant row, requires it to be active and
// returns it so the reservation can pin every revision.
func (s *Service) activeGrant(ctx context.Context, hostID, deviceID string) (store.AdmissionGrant, error) {
	grant, err := s.store.AdmissionGrantFor(ctx, hostID, deviceID)
	if err != nil {
		if errors.Is(err, store.ErrNotFound) {
			return store.AdmissionGrant{}, ErrForbidden
		}
		return store.AdmissionGrant{}, err
	}
	if !grant.Active || grant.HostID != hostID || grant.DeviceID != deviceID {
		return store.AdmissionGrant{}, ErrForbidden
	}
	return grant, nil
}

// checkGrant rechecks the current revisions on every ciphertext release. A
// mismatch means the original grant was revoked or re-granted; the old
// reference stays fenced and no bytes are released.
func (s *Service) checkGrant(ctx context.Context, object store.PayloadObject) error {
	if object.GrantGeneration == nil || object.RevocationGeneration == nil || object.DeviceRevocationGeneration == nil {
		return ErrForbidden
	}
	if *object.GrantGeneration < 0 || *object.RevocationGeneration < 0 || *object.DeviceRevocationGeneration < 0 {
		return ErrForbidden
	}
	grant, err := s.store.AdmissionGrantFor(ctx, object.HostID, object.DeviceID)
	if err != nil {
		if errors.Is(err, store.ErrNotFound) {
			return ErrForbidden
		}
		return err
	}
	if !grant.Active ||
		grant.GrantGeneration != uint64(*object.GrantGeneration) ||
		grant.RevocationGeneration != uint64(*object.RevocationGeneration) ||
		grant.DeviceRevocationGeneration != uint64(*object.DeviceRevocationGeneration) {
		return ErrForbidden
	}
	if object.EnrollmentGeneration != nil &&
		(*object.EnrollmentGeneration < 0 || uint64(*object.EnrollmentGeneration) != grant.EnrollmentGeneration) {
		return ErrForbidden
	}
	return nil
}

func (s *Service) checkOwner(requested string) error {
	requested = strings.TrimSpace(requested)
	if requested == "" {
		return nil
	}
	if requested != s.config.owner() {
		return &WrongOwnerError{Requested: requested}
	}
	return nil
}

func refFor(object store.PayloadObject) payload.Ref {
	return payload.Ref{
		ObjectID:   object.ObjectID,
		HostID:     object.HostID,
		DeviceID:   object.DeviceID,
		AppSession: object.AppSession,
		ChannelID:  object.ChannelID,
		Purpose:    object.Purpose,
	}
}

func objectResult(object payload.Object) Object {
	return Object{
		ObjectID:        object.ObjectID,
		OwnerNodeID:     object.OwnerID,
		Purpose:         purposeValue(object.Purpose),
		CiphertextBytes: uint64(object.Length),
		SHA256:          append([]byte(nil), object.Digest...),
		ExpiresAtUnixMs: object.ExpiresAt.UnixMilli(),
	}
}

// purposeName is the wire enum name stored with the reservation. It is also
// the canonical reserve signature component, so a stored value always matches
// what the Host signed.
func purposeName(purpose whalereinv1.PayloadPurpose) string {
	if name, ok := whalereinv1.PayloadPurpose_name[int32(purpose)]; ok && purpose != 0 {
		return name
	}
	return ""
}

func purposeValue(name string) whalereinv1.PayloadPurpose {
	if value, ok := whalereinv1.PayloadPurpose_value[name]; ok {
		return whalereinv1.PayloadPurpose(value)
	}
	return whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_UNSPECIFIED
}

func int64Ptr(value uint64) *int64 {
	if value > math.MaxInt64 {
		return nil
	}
	converted := int64(value)
	return &converted
}

func newObjectID() (string, error) {
	raw := make([]byte, 16)
	if _, err := rand.Read(raw); err != nil {
		return "", fmt.Errorf("relay object id: %w", err)
	}
	return hex.EncodeToString(raw), nil
}

func validUTF8Component(name, value string, max int) error {
	if len(value) > max {
		return fmt.Errorf("%w: %s is too long", ErrInvalid, name)
	}
	if strings.ContainsAny(value, "|\x00\r\n") {
		return fmt.Errorf("%w: %s contains an unsafe character", ErrInvalid, name)
	}
	for _, r := range value {
		if r < 0x20 || r == 0x7f {
			return fmt.Errorf("%w: %s contains a control character", ErrInvalid, name)
		}
	}
	return nil
}

// translate maps payload lifecycle errors onto the stable relay outcomes. Raw
// database errors pass through and are reported as internal errors.
func translate(err error) error {
	switch {
	case err == nil:
		return nil
	case errors.Is(err, payload.ErrDisabled):
		return ErrDisabled
	case errors.Is(err, payload.ErrInvalid):
		return fmt.Errorf("%w: payload request is invalid", ErrInvalid)
	case errors.Is(err, payload.ErrTooLarge):
		return ErrTooLarge
	case errors.Is(err, payload.ErrQuota), errors.Is(err, payload.ErrCapacity):
		return ErrQuota
	case errors.Is(err, payload.ErrHeadroom):
		return ErrPressure
	case errors.Is(err, payload.ErrDuplicate), errors.Is(err, payload.ErrNotReady),
		errors.Is(err, payload.ErrNotStaged), errors.Is(err, payload.ErrLengthMismatch),
		errors.Is(err, store.ErrConflict):
		return ErrConflict
	case errors.Is(err, payload.ErrNotFound), errors.Is(err, store.ErrNotFound):
		return ErrNotFound
	case errors.Is(err, payload.ErrExpired):
		return ErrExpired
	case errors.Is(err, payload.ErrForbidden), errors.Is(err, store.ErrPayloadScopeMismatch):
		return ErrForbidden
	case errors.Is(err, payload.ErrDigestMismatch):
		return ErrDigestMismatch
	case errors.Is(err, io.ErrUnexpectedEOF):
		// A client that stops before its declared Content-Length has not
		// completed the reserved upload; it is a conflict, not a server fault.
		return fmt.Errorf("%w: upload body ended early", ErrConflict)
	case errors.Is(err, context.DeadlineExceeded), errors.Is(err, context.Canceled):
		return ErrUnavailable
	default:
		return err
	}
}

// translateAuth maps the auth service's typed denials onto relay outcomes.
func translateAuth(err error) error {
	if err == nil {
		return nil
	}
	var denial *auth.AuthorityDenial
	if errors.As(err, &denial) {
		if denial.StatusCode == 503 {
			return fmt.Errorf("%w: %s", ErrUnavailable, denial.Reason)
		}
		return ErrForbidden
	}
	switch {
	case errors.Is(err, auth.ErrUnauthorized), errors.Is(err, auth.ErrSignatureExpired):
		// A signature outside the window is a bad Host signature on this
		// surface: the wire contract maps both to 401 unauthorized.
		return ErrUnauthorized
	case errors.Is(err, auth.ErrBadRequest):
		return fmt.Errorf("%w: %s", ErrInvalid, err.Error())
	case errors.Is(err, auth.ErrForbidden):
		return ErrForbidden
	case errors.Is(err, auth.ErrUnavailable):
		return ErrUnavailable
	default:
		return err
	}
}

// observeRelay emits one ciphertext object boundary. The object token is the
// scoped object identity; the request token is the parent when one exists and
// the object's own message identity for a pre-admission body.
func (s *Service) observeRelay(scope, request, object string, stage observation.RelayStage, outcome observation.Outcome, bytes int) {
	emitObjectFact(s.observation, scope, request, object, stage, outcome, bytes)
}

// emitObjectFact is shared by the HTTP relay service and the broker stager.
// Both carry reference-route object boundaries; only the sink owner differs.
func emitObjectFact(sink *observation.Sink, scope, request, object string, stage observation.RelayStage, outcome observation.Outcome, bytes int) {
	if sink == nil {
		return
	}
	event := observation.RelayEvent{
		Scope: scope, Request: request, ObjectOp: object, Stage: stage, Route: observation.RouteReference, Outcome: outcome,
	}
	if bytes > 0 {
		event.Bytes = uint64(bytes)
		event.HasBytes = true
	}
	sink.Relay(event)
}

func relayRequestID(parent, object string) string {
	if parent != "" {
		return parent
	}
	return object
}

// countingReader counts physical bytes the payload service actually consumed
// from the request body. It never inspects content.
type countingReader struct {
	reader io.Reader
	count  int
}

func (r *countingReader) Read(buffer []byte) (int, error) {
	n, err := r.reader.Read(buffer)
	r.count += n
	return n, err
}

func relayOutcome(err error) observation.Outcome {
	return observation.OutcomeForError(err, ErrExpired)
}

// relayScope binds an object's parent request token to the authenticated
// host/device scope captured at reservation.
func relayScope(hostID, deviceID string) string {
	return observation.Scope(hostID, deviceID)
}
