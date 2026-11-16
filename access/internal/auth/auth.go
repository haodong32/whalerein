// Package auth owns transport identity, database-derived authorization and
// revocation for WhaleRein Access.
//
// The package never imports the broker package. Broker authentication and ACL
// callbacks are wired in cmd/server through the interfaces declared here.
package auth

import (
	"context"
	"crypto/sha256"
	"crypto/subtle"
	"errors"
	"fmt"
	"log/slog"
	"regexp"
	"strings"
	"sync"
	"time"

	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/policy"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// Stable authorization failures.
var (
	ErrUnauthorized     = errors.New("unauthorized")
	ErrForbidden        = errors.New("forbidden")
	ErrNotFound         = errors.New("not found")
	ErrBadRequest       = errors.New("invalid credentials")
	ErrUnavailable      = errors.New("unavailable")
	ErrSignatureExpired = errors.New("host signature expired")
)

// PrincipalKind is the broker role of an authenticated client.
type PrincipalKind string

// Broker roles.
const (
	KindAccess PrincipalKind = "access"
	KindHost   PrincipalKind = "host"
	KindDevice PrincipalKind = "device"
	KindProbe  PrincipalKind = "probe"
)

var probeClientID = regexp.MustCompile(`^whalerein-probe-[0-9a-f]{32}$`)
var probeHostID = regexp.MustCompile(`^[A-Za-z0-9_.:-]{1,128}$`)

// Identity is an HTTP- or MQTT-authenticated device.
type Identity struct {
	DeviceID string
}

// BrokerIdentity is an authenticated broker principal.
type BrokerIdentity struct {
	Kind     PrincipalKind
	HostID   string
	DeviceID string
}

// HostView is the device-visible host record plus broker presence.
type HostView struct {
	HostID          string
	Fingerprint     string
	ProtocolVersion uint32
	Online          bool
}

// Store is the subset of store.Store used by auth.
type Store interface {
	EnsureHost(ctx context.Context, hostID string) (store.Host, error)
	GetHost(ctx context.Context, hostID string) (store.Host, error)
	ListAuthorizedHosts(ctx context.Context, deviceID string) ([]store.Host, error)
	ListAuthorizedHostsPage(ctx context.Context, deviceID string, limit, offset int) ([]store.Host, error)
	ActiveDeviceByAuthHash(ctx context.Context, authHash []byte) (store.Device, error)
	DeviceByID(ctx context.Context, deviceID string) (store.Device, error)
	HasActiveGrant(ctx context.Context, hostID, deviceID string) (bool, error)
	ListDeviceGrants(ctx context.Context, deviceID string) ([]store.AdmissionGrant, error)
	RevokeDeviceGrants(ctx context.Context, deviceID string, at time.Time) ([]string, bool, error)
	RevokeGrant(ctx context.Context, hostID, deviceID string, at time.Time) error
	SetHostAuthorizationKey(ctx context.Context, hostID string, publicKey []byte) error
	HostAuthorizationKey(ctx context.Context, hostID string) ([]byte, error)
	PendingRevocations(ctx context.Context, hostID string) ([]store.Revocation, error)
	MarkRevocationsReplayed(ctx context.Context, hostID string, deviceIDs []string, revokedBefore, at time.Time, generations map[string]uint64) error
}

// Admission is the bounded local grant authority used by realtime checks.
// PostgreSQL remains authoritative and reconciliation repairs local state.
type Admission interface {
	Authorize(ctx context.Context, hostID, deviceID string) (bool, error)
	Deny(hostID, deviceID string)
	DenyDevice(deviceID string)
	CompleteDeviceRevoke(deviceID string)
	Allow(ctx context.Context, hostID, deviceID string) error
}

// PolicyObserver records a policy revision that the broker already installed
// for a connection, so reconciliation can skip unchanged projections.
type PolicyObserver interface {
	RecordPolicy(deviceID string, revision uint64)
}

// LineEnforcer applies revocation to the broker policy and closes superseded
// broker connections. A revocation response is only complete after this
// succeeds.
type LineEnforcer interface {
	RevokeGrant(ctx context.Context, hostID, deviceID string) error
	RevokeDevice(ctx context.Context, deviceID string) error
}

// Presence is broker-derived host connectivity. It never reads the database.
type Presence interface {
	IsHostOnline(ctx context.Context, hostID string) bool
}

// Transport publishes host requests and performs explicit disconnects. The
// broker package implements it; auth does not depend on that package.
type Transport interface {
	PublishAndAwait(ctx context.Context, hostID string, req *whalereinv1.HostRequest, timeout time.Duration) (*whalereinv1.HostResult, error)
	Publish(ctx context.Context, hostID string, req *whalereinv1.HostRequest) error
	DisconnectDevice(ctx context.Context, deviceID string) error
}

// Config holds auth-relevant settings. Tokens are never logged.
type Config struct {
	AdminToken            string
	HostProvisioningToken string
	AccessUsername        string
	AccessPassword        string
	ProtocolVersion       uint32
	RevocationAckTimeout  time.Duration
}

// Dependencies wires the auth service.
type Dependencies struct {
	Store     Store
	Admission Admission
	Enforcer  LineEnforcer
	Observer  PolicyObserver
	Presence  Presence
	Transport Transport
	Config    Config
	Metrics   *diagnostics.Metrics
	Masker    *diagnostics.Masker
	Logger    *slog.Logger
	Now       func() time.Time
}

// Service implements transport authorization and revocation.
type Service struct {
	store     Store
	admission Admission
	enforcer  LineEnforcer
	observer  PolicyObserver
	presence  Presence
	transport Transport
	cfg       Config
	metrics   *diagnostics.Metrics
	masker    *diagnostics.Masker
	logger    *slog.Logger
	now       func() time.Time

	seenMu sync.Mutex
	seen   map[string]int64
}

// New constructs an auth service.
func New(deps Dependencies) (*Service, error) {
	if deps.Store == nil {
		return nil, errors.New("auth: store is required")
	}
	if deps.Admission == nil {
		return nil, errors.New("auth: admission is required")
	}
	if deps.Enforcer == nil {
		return nil, errors.New("auth: line enforcer is required")
	}
	if deps.Presence == nil {
		return nil, errors.New("auth: presence is required")
	}
	if deps.Transport == nil {
		return nil, errors.New("auth: transport is required")
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
	return &Service{
		store:     deps.Store,
		admission: deps.Admission,
		enforcer:  deps.Enforcer,
		observer:  deps.Observer,
		presence:  deps.Presence,
		transport: deps.Transport,
		cfg:       deps.Config,
		metrics:   deps.Metrics,
		masker:    deps.Masker,
		logger:    deps.Logger,
		now:       deps.Now,
		seen:      map[string]int64{},
	}, nil
}

// HashToken returns SHA-256(token), the only representation Access stores for
// pairing and device transport credentials.
func HashToken(token string) []byte {
	sum := sha256.Sum256([]byte(token))
	return sum[:]
}

func tokenEqual(a, b string) bool {
	left := sha256.Sum256([]byte(a))
	right := sha256.Sum256([]byte(b))
	return subtle.ConstantTimeCompare(left[:], right[:]) == 1
}

// AuthenticateAdmin checks the configured admin bearer token.
func (s *Service) AuthenticateAdmin(token string) error {
	if token == "" || !tokenEqual(token, s.cfg.AdminToken) {
		return ErrUnauthorized
	}
	return nil
}

// AuthenticateDeviceToken authenticates a device transport token and requires
// at least one live grant.
func (s *Service) AuthenticateDeviceToken(ctx context.Context, token string) (Identity, error) {
	if strings.TrimSpace(token) == "" {
		return Identity{}, ErrUnauthorized
	}
	device, err := s.store.ActiveDeviceByAuthHash(ctx, HashToken(token))
	if err != nil {
		if errors.Is(err, store.ErrNotFound) {
			return Identity{}, ErrUnauthorized
		}
		return Identity{}, fmt.Errorf("authenticate device: %w", err)
	}
	return Identity{DeviceID: device.DeviceID}, nil
}

// AuthorizeHost requires a live grant from the device to the host. Warmed
// decisions read only local admission state.
func (s *Service) AuthorizeHost(ctx context.Context, identity Identity, hostID string) error {
	active, err := s.admission.Authorize(ctx, hostID, identity.DeviceID)
	if err != nil {
		return fmt.Errorf("authorize host: %w", err)
	}
	if !active {
		return ErrForbidden
	}
	return nil
}

// ListHosts returns one bounded page of the hosts the device may access and
// their current broker presence. truncated reports that another page exists.
func (s *Service) ListHosts(ctx context.Context, identity Identity, limit, offset int) ([]HostView, bool, error) {
	if limit <= 0 {
		return nil, false, ErrBadRequest
	}
	if offset < 0 {
		return nil, false, ErrBadRequest
	}
	hosts, err := s.store.ListAuthorizedHostsPage(ctx, identity.DeviceID, limit+1, offset)
	if err != nil {
		return nil, false, fmt.Errorf("list hosts: %w", err)
	}
	truncated := len(hosts) > limit
	if truncated {
		hosts = hosts[:limit]
	}
	views := make([]HostView, 0, len(hosts))
	for _, host := range hosts {
		views = append(views, HostView{
			HostID:          host.HostID,
			Fingerprint:     host.Fingerprint,
			ProtocolVersion: host.ProtocolVersion,
			Online:          s.presence.IsHostOnline(ctx, host.HostID),
		})
	}
	return views, truncated, nil
}

// IdentityFromCredentials parses the broker username/client id convention
// without validating the password.
func validDeviceClientSuffix(suffix string) bool {
	if suffix == "" || len(suffix) > 32 {
		return false
	}
	for _, character := range suffix {
		if !(character >= 'a' && character <= 'z') && !(character >= '0' && character <= '9') {
			return false
		}
	}
	return true
}

func (s *Service) IdentityFromCredentials(clientID, username string) (BrokerIdentity, error) {
	switch {
	case username == s.cfg.AccessUsername && username != "":
		if clientID == "" {
			return BrokerIdentity{}, ErrBadRequest
		}
		return BrokerIdentity{Kind: KindAccess}, nil
	case strings.HasPrefix(username, "probe:"):
		hostID := strings.TrimPrefix(username, "probe:")
		if !probeClientID.MatchString(clientID) || !probeHostID.MatchString(hostID) {
			return BrokerIdentity{}, ErrBadRequest
		}
		return BrokerIdentity{Kind: KindProbe, HostID: hostID}, nil
	case strings.HasPrefix(username, "host:"):
		hostID := strings.TrimPrefix(username, "host:")
		if hostID == "" || clientID != "whalerein-host-"+hostID {
			return BrokerIdentity{}, ErrBadRequest
		}
		return BrokerIdentity{Kind: KindHost, HostID: hostID}, nil
	case strings.HasPrefix(username, "device:"):
		deviceID := strings.TrimPrefix(username, "device:")
		prefix := "whalerein-device-" + deviceID
		// A device may open more than one broker session (a live watch and a
		// send in another process). The username still carries the device id;
		// an optional bounded suffix keeps the sessions distinct without
		// letting one device assume another's identity.
		validClient := clientID == prefix || (strings.HasPrefix(clientID, prefix+"-") && validDeviceClientSuffix(clientID[len(prefix)+1:]))
		if deviceID == "" || !validClient {
			return BrokerIdentity{}, ErrBadRequest
		}
		return BrokerIdentity{Kind: KindDevice, DeviceID: deviceID}, nil
	default:
		return BrokerIdentity{}, ErrUnauthorized
	}
}

// AuthenticateBroker validates a broker CONNECT request from the native plugin.
// A host is inserted on first successful authentication.
func (s *Service) AuthenticateBroker(ctx context.Context, clientID, username, password string) (BrokerIdentity, error) {
	identity, err := s.IdentityFromCredentials(clientID, username)
	if err != nil {
		return BrokerIdentity{}, err
	}
	switch identity.Kind {
	case KindAccess:
		if !tokenEqual(password, s.cfg.AccessPassword) {
			return BrokerIdentity{}, ErrUnauthorized
		}
		return identity, nil
	case KindProbe:
		// Probe the same credential without provisioning a host or granting topic
		// access. The ACL default denies every operation for this role.
		if !tokenEqual(password, s.cfg.HostProvisioningToken) {
			return BrokerIdentity{}, ErrUnauthorized
		}
		return identity, nil
	case KindHost:
		if !tokenEqual(password, s.cfg.HostProvisioningToken) {
			return BrokerIdentity{}, ErrUnauthorized
		}
		if _, err := s.store.EnsureHost(ctx, identity.HostID); err != nil {
			return BrokerIdentity{}, fmt.Errorf("provision host: %w", err)
		}
		return identity, nil
	case KindDevice:
		device, err := s.store.ActiveDeviceByAuthHash(ctx, HashToken(password))
		if err != nil {
			if errors.Is(err, store.ErrNotFound) {
				return BrokerIdentity{}, ErrUnauthorized
			}
			return BrokerIdentity{}, fmt.Errorf("authenticate device broker: %w", err)
		}
		if device.DeviceID != identity.DeviceID {
			return BrokerIdentity{}, ErrUnauthorized
		}
		return identity, nil
	default:
		return BrokerIdentity{}, ErrUnauthorized
	}
}

// BrokerPolicy projects the committed grants for one authenticated broker
// principal. Device policies read the durable grant list; the result is
// installed by the broker plugin for the life of that connection.
func (s *Service) BrokerPolicy(ctx context.Context, identity BrokerIdentity) (policy.Connection, error) {
	if identity.Kind != KindDevice {
		return policy.Build(string(identity.Kind), identity.HostID, identity.DeviceID, nil), nil
	}
	grants, err := s.store.ListDeviceGrants(ctx, identity.DeviceID)
	if err != nil {
		return policy.Connection{}, fmt.Errorf("broker policy: %w", err)
	}
	connection := policy.Build(string(identity.Kind), identity.HostID, identity.DeviceID, grants)
	if s.observer != nil {
		s.observer.RecordPolicy(identity.DeviceID, connection.Revision)
	}
	return connection, nil
}

// AuthenticateAccessCredentials is used by tests and by configuration checks.
func (s *Service) AuthenticateAccessCredentials(username, password string) bool {
	return username == s.cfg.AccessUsername && tokenEqual(password, s.cfg.AccessPassword)
}
