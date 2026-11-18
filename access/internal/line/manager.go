// Package line projects committed grants onto the Mosquitto line. Access is the
// only policy writer; this package builds the broker policy, acknowledges
// application, reconciles after failure and keeps presence ordered.
package line

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/whalerein/whalerein/access/internal/broker"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/policy"
	"github.com/whalerein/whalerein/access/internal/store"
)

// Store is the durable grant source used for projection and reconciliation.
type Store interface {
	ListAdmissionGrants(ctx context.Context) ([]store.AdmissionGrant, error)
	ListDeviceGrants(ctx context.Context, deviceID string) ([]store.AdmissionGrant, error)
	AdmissionGrantFor(ctx context.Context, hostID, deviceID string) (store.AdmissionGrant, error)
}

// Applier is the admission boundary updated after a confirmed pairing.
type Applier interface {
	Allow(ctx context.Context, hostID, deviceID string) error
}

// Config bounds broker policy work.
type Config struct {
	// PolicyTTL is passed to the broker with each policy update. It must stay
	// below the broker's configured validity window.
	PolicyTTL time.Duration
	// Timeout bounds one control operation.
	Timeout time.Duration
}

// Manager owns broker policy projection and reconciliation.
type Manager struct {
	sessionPolicy func(context.Context, string, string) (policy.Connection, error)
	store         Store
	control       *broker.Control
	applier       Applier
	presence      *broker.Presence
	cfg           Config
	metrics       *diagnostics.Metrics
	masker        *diagnostics.Masker
	logger        *slog.Logger

	appliedMu sync.Mutex
	applied   map[string]uint64
}

// Dependencies wires a line manager.
type Dependencies struct {
	Store     Store
	Control   *broker.Control
	Admission Applier
	Presence  *broker.Presence
	Config    Config
	Metrics   *diagnostics.Metrics
	Masker    *diagnostics.Masker
	Logger    *slog.Logger
}

// New constructs a line manager.
func New(deps Dependencies) (*Manager, error) {
	if deps.Store == nil || deps.Control == nil || deps.Admission == nil || deps.Presence == nil {
		return nil, errors.New("line: store, control, admission and presence are required")
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
	if deps.Config.PolicyTTL <= 0 {
		deps.Config.PolicyTTL = 10 * time.Second
	}
	if deps.Config.Timeout <= 0 {
		deps.Config.Timeout = 5 * time.Second
	}
	return &Manager{
		store:    deps.Store,
		control:  deps.Control,
		applier:  deps.Admission,
		presence: deps.Presence,
		cfg:      deps.Config,
		metrics:  deps.Metrics,
		masker:   deps.Masker,
		logger:   deps.Logger,
		applied:  map[string]uint64{},
	}, nil
}

// SetSessionPolicy wires managed projection before the broker starts.
func (m *Manager) SetSessionPolicy(project func(context.Context, string, string) (policy.Connection, error)) {
	m.sessionPolicy = project
}

func (m *Manager) request(ctx context.Context, op string, fields ...string) (*broker.ControlReply, error) {
	requestCtx, cancel := context.WithTimeout(ctx, m.cfg.Timeout)
	defer cancel()
	return m.control.Request(requestCtx, op, fields)
}

// Reconcile applies the broker snapshot, refreshes the policy validity window
// and projects current durable policy. It must succeed before external traffic
// is admitted.
func (m *Manager) Reconcile(ctx context.Context) error {
	snapshot, err := m.request(ctx, "snapshot")
	if err != nil {
		m.metrics.AdmissionReconcileErrors.Inc()
		return fmt.Errorf("line: broker snapshot: %w", err)
	}
	if m.sessionPolicy != nil && snapshot.Fields["session_capability"] != "1" {
		return errors.New("line: broker does not support managed device sessions")
	}
	m.presence.ApplySnapshot(snapshot.Clients, snapshot.Sequence)

	/* Only live broker connections need a policy projection. Historical or
	 * revoked grants must not turn reconciliation into an O(all grants) sweep
	 * that competes with forwarding, so group durable grants once and then
	 * visit connected device clients only. */
	grants, err := m.store.ListAdmissionGrants(ctx)
	if err != nil {
		m.metrics.AdmissionReconcileErrors.Inc()
		return fmt.Errorf("line: list grants: %w", err)
	}
	grouped := map[string][]store.AdmissionGrant{}
	var revision uint64
	for _, grant := range grants {
		if !grant.Active {
			continue
		}
		grouped[grant.DeviceID] = append(grouped[grant.DeviceID], grant)
		if value := policy.Revision(grant); value > revision {
			revision = value
		}
	}
	projected := 0
	renewed := make(map[string]bool)
	for _, client := range snapshot.Clients {
		if client.Kind != policy.KindDevice || client.State != "connected" || client.DeviceID == "" {
			continue
		}
		if client.SessionID != "" {
			if m.sessionPolicy == nil {
				return errors.New("line: managed connection without a session policy source")
			}
			connection, err := m.sessionPolicy(ctx, client.SessionID, client.DeviceID)
			if err != nil {
				return fmt.Errorf("line: project managed connection: %w", err)
			}
			if connection.SessionID == "" {
				if _, err := m.request(ctx, "session_absent", "session "+client.SessionID); err != nil {
					return err
				}
				continue
			}
			if !renewed[client.SessionID] {
				if _, err := m.request(ctx, "renew_session", "session "+client.SessionID,
					fmt.Sprintf("session_revision %d", connection.SessionRevision),
					fmt.Sprintf("deadline_ms %d", connection.SessionExpiresAt)); err != nil {
					return err
				}
				renewed[client.SessionID] = true
			}
			if connection.Revision > client.Revision {
				if _, err := m.request(ctx, "set", policy.ControlMessages(connection, client.ClientID, int(m.cfg.PolicyTTL.Milliseconds()))...); err != nil && !isNotConnected(err) {
					return err
				}
			}
			continue
		}
		deviceGrants := grouped[client.DeviceID]
		if len(deviceGrants) == 0 {
			// A connected device with no live grant is stale; clear its policy
			// and close it rather than leaving an unqualified connection.
			m.forget(client.DeviceID)
			if _, err := m.request(ctx, "remove", "client "+client.ClientID, "kick 1"); err != nil && !isNotConnected(err) {
				m.metrics.AdmissionReconcileErrors.Inc()
				return fmt.Errorf("line: remove stale device policy: %w", err)
			}
			continue
		}
		connection := policy.Build(policy.KindDevice, "", client.DeviceID, deviceGrants)
		if m.currentlyApplied(client.DeviceID, connection.Revision) {
			continue
		}
		if _, err := m.request(ctx, "set", policy.ControlMessages(connection, client.ClientID, int(m.cfg.PolicyTTL.Milliseconds()))...); err != nil {
			if !isNotConnected(err) {
				m.metrics.AdmissionReconcileErrors.Inc()
				return fmt.Errorf("line: set device policy: %w", err)
			}
			continue
		}
		m.recordApplied(client.DeviceID, connection.Revision)
		projected++
	}
	if _, err := m.request(ctx, "ping", fmt.Sprintf("revision %d", revision), fmt.Sprintf("ttl %d", m.cfg.PolicyTTL.Milliseconds())); err != nil {
		m.metrics.AdmissionReconcileErrors.Inc()
		return fmt.Errorf("line: ping: %w", err)
	}
	m.logger.Info("line policy reconciled", "connected_devices", len(snapshot.Clients), "projected", projected, "revision", revision)
	return nil
}

func (m *Manager) currentlyApplied(deviceID string, revision uint64) bool {
	m.appliedMu.Lock()
	defer m.appliedMu.Unlock()
	return m.applied[deviceID] == revision
}

func (m *Manager) recordApplied(deviceID string, revision uint64) {
	m.appliedMu.Lock()
	m.applied[deviceID] = revision
	m.appliedMu.Unlock()
}

// RecordPolicy records a revision the broker installed when a connection
// authenticated, so the next reconcile does not repeat an identical projection.
func (m *Manager) RecordPolicy(deviceID string, revision uint64) {
	if deviceID == "" {
		return
	}
	m.recordApplied(deviceID, revision)
}

func (m *Manager) forget(deviceID string) {
	m.appliedMu.Lock()
	delete(m.applied, deviceID)
	m.appliedMu.Unlock()
}

// Heartbeat refreshes the broker policy validity window. Without it the broker
// fails closed after the validity window, which is the intended partition
// behavior. It also expires presence observations that outlived the cache TTL,
// so a lost disconnect cannot leave a Host or device online indefinitely when
// the snapshot reconcile loop is stalled.
func (m *Manager) Heartbeat(ctx context.Context) error {
	m.presence.Sweep()
	if m.sessionPolicy != nil {
		return m.Reconcile(ctx)
	}
	_, err := m.request(ctx, "ping", "ttl "+fmt.Sprint(m.cfg.PolicyTTL.Milliseconds()))
	return err
}

// ApplyPairing installs the confirmed grant into local admission before the
// pairing response is returned. The broker projection is best effort because a
// device receives its committed policy when it authenticates; doing a control
// round trip here delays the pairing COMMIT and can race the durable outbox.
func (m *Manager) ApplyPairing(ctx context.Context, hostID, deviceID string) error {
	if err := m.applier.Allow(ctx, hostID, deviceID); err != nil {
		return fmt.Errorf("line: apply pairing admission: %w", err)
	}
	go m.applyDevicePolicy(deviceID)
	return nil
}

func (m *Manager) applyDevicePolicy(deviceID string) {
	ctx, cancel := context.WithTimeout(context.Background(), m.cfg.Timeout)
	defer cancel()
	grants, err := m.store.ListDeviceGrants(ctx, deviceID)
	if err != nil {
		m.metrics.AdmissionReconcileErrors.Inc()
		m.logger.Warn("line: pairing policy read failed", "error", err.Error())
		return
	}
	connection := policy.Build(policy.KindDevice, "", deviceID, grants)
	if m.currentlyApplied(deviceID, connection.Revision) {
		return
	}
	if _, err := m.request(ctx, "set", policy.ControlMessages(connection, broker.ClientIDForDevice(deviceID), int(m.cfg.PolicyTTL.Milliseconds()))...); err != nil {
		if !isNotConnected(err) {
			m.metrics.AdmissionReconcileErrors.Inc()
			m.logger.Warn("line: pairing policy projection failed", "error", err.Error())
		}
		return
	}
	m.recordApplied(deviceID, connection.Revision)
}

// RevokeGrant fences the Host/device scope on the broker and closes the
// superseded connection.
func (m *Manager) RevokeGrant(ctx context.Context, hostID, deviceID string) error {
	grant, err := m.store.AdmissionGrantFor(ctx, hostID, deviceID)
	if err != nil {
		return fmt.Errorf("line: read revoked grant: %w", err)
	}
	m.forget(deviceID)
	if _, err := m.request(ctx, "deny",
		"device "+deviceID,
		fmt.Sprintf("generation %d", policy.Revision(grant)),
		"host "+hostID,
		"kick 1",
	); err != nil {
		return fmt.Errorf("line: deny grant: %w", err)
	}
	return nil
}

// RevokeDevice fences every Host scope of a device-wide revocation and closes
// the device connection.
func (m *Manager) RevokeDevice(ctx context.Context, deviceID string) error {
	m.forget(deviceID)
	grants, err := m.store.ListDeviceGrants(ctx, deviceID)
	if err != nil {
		return fmt.Errorf("line: read device grants: %w", err)
	}
	for _, revoked := range grants {
		hostID := revoked.HostID
		grant, err := m.store.AdmissionGrantFor(ctx, hostID, deviceID)
		if err != nil {
			return fmt.Errorf("line: read revoked grant: %w", err)
		}
		if _, err := m.request(ctx, "deny",
			"device "+deviceID,
			fmt.Sprintf("generation %d", policy.Revision(grant)),
			"host "+hostID,
			"kick 0",
		); err != nil {
			return fmt.Errorf("line: deny device grant: %w", err)
		}
	}
	if _, err := m.request(ctx, "kick", "username device:"+deviceID); err != nil {
		return fmt.Errorf("line: disconnect device: %w", err)
	}
	return nil
}

// RevokeHost denies every revoked grant incarnation of one disabled Host and
// closes its live broker session. It is idempotent: an already closed session
// is not an error, and an empty revocation set still closes the Host.
func (m *Manager) RevokeHost(ctx context.Context, hostID string, grantGenerations map[string]uint64) error {
	deviceIDs := make([]string, 0, len(grantGenerations))
	for deviceID := range grantGenerations {
		deviceIDs = append(deviceIDs, deviceID)
	}
	// Map iteration is unordered; sort so a retry issues the same control
	// operations in the same order.
	sort.Strings(deviceIDs)
	for _, deviceID := range deviceIDs {
		m.forget(deviceID)
		if _, err := m.request(ctx, "deny",
			"device "+deviceID,
			fmt.Sprintf("generation %d", grantGenerations[deviceID]),
			"host "+hostID,
			"kick 1",
		); err != nil && !isNotConnected(err) {
			return fmt.Errorf("line: deny disabled host grant: %w", err)
		}
	}
	if _, err := m.request(ctx, "kick", "username host:"+hostID); err != nil && !isNotConnected(err) {
		return fmt.Errorf("line: close disabled host session: %w", err)
	}
	return nil
}

func isNotConnected(err error) bool {
	if err == nil {
		return false
	}
	text := strings.ToLower(err.Error())
	return strings.Contains(text, "not_found") || strings.Contains(text, "client not connected")
}

// ErrUnavailable reports that the broker cannot confirm policy.
var ErrUnavailable = errors.New("line: broker policy unavailable")
