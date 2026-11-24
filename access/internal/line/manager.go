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
	if snapshot.Epoch == "" {
		return errors.New("line: broker does not report a process epoch")
	}
	m.presence.SetBrokerEpoch(snapshot.Epoch)
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

// RevokeGrant fences exactly one frozen Host/device grant scope and closes
// only the admitted connections that carried it. A delayed retry re-issues the
// stored generations, so a newer grant or a same-id replacement is untouched.
func (m *Manager) RevokeGrant(ctx context.Context, scope store.RevocationScope) (store.EnforcementReceipt, error) {
	return m.enforce(ctx, scope)
}

// RevokeDevice fences every frozen Host scope of a device-wide revocation and
// closes only the admitted device connections observed for that scope.
func (m *Manager) RevokeDevice(ctx context.Context, scope store.RevocationScope) (store.EnforcementReceipt, error) {
	return m.enforce(ctx, scope)
}

// RevokeHost denies every frozen grant incarnation of one disabled Host and
// closes its admitted Host connection. It is idempotent: an already closed
// session is not an error, and an empty revocation set still fences the scope.
func (m *Manager) RevokeHost(ctx context.Context, scope store.RevocationScope) (store.EnforcementReceipt, error) {
	return m.enforce(ctx, scope)
}

// enforce applies one frozen revocation scope. The base deny installs the
// generation fence even when every frozen connection is already gone; the
// conn-scoped deny closes the exact admitted instance. An absent or replaced
// connection returns ok/absent and counts as enforced for the old scope.
func (m *Manager) enforce(ctx context.Context, frozen store.RevocationScope) (store.EnforcementReceipt, error) {
	scope := frozen.Frozen()
	receipt := store.EnforcementReceipt{Scope: scope}
	grants := append([]store.ScopeGrant(nil), scope.Grants...)
	sort.SliceStable(grants, func(i, j int) bool {
		if grants[i].HostID != grants[j].HostID {
			return grants[i].HostID < grants[j].HostID
		}
		if grants[i].DeviceID != grants[j].DeviceID {
			return grants[i].DeviceID < grants[j].DeviceID
		}
		return grants[i].GrantGeneration < grants[j].GrantGeneration
	})
	for _, grant := range grants {
		deviceID := grant.DeviceID
		if deviceID == "" {
			deviceID = scope.DeviceID
		}
		if grant.HostID == "" || deviceID == "" {
			continue
		}
		m.forget(deviceID)
		base := []string{
			"device " + deviceID,
			fmt.Sprintf("generation %d", grant.Revision()),
			"host " + grant.HostID,
			"kick 0",
		}
		if _, err := m.request(ctx, "deny", base...); err != nil && !isNotConnected(err) {
			return receipt, fmt.Errorf("line: fence revoked grant: %w", err)
		}
		for _, connection := range m.currentConnections(grant.Connections) {
			reply, err := m.request(ctx, "deny",
				"device "+deviceID,
				fmt.Sprintf("generation %d", grant.Revision()),
				"host "+grant.HostID,
				fmt.Sprintf("conn %d", connection.Ref.ID),
				"kick 1",
			)
			if err != nil {
				if isNotConnected(err) {
					continue
				}
				return receipt, fmt.Errorf("line: deny revoked grant: %w", err)
			}
			if reply.Fields["matched"] != "0" {
				receipt.Applied = appendScopeConnection(receipt.Applied, connection)
			}
		}
	}
	if scope.HostConnection != nil && scope.HostConnection.Valid() {
		host := scope.HostConnection
		reply, err := m.request(ctx, "remove",
			"client "+host.ClientID,
			fmt.Sprintf("conn %d", host.Ref.ID),
			"kick 1",
		)
		if err != nil {
			if !isNotConnected(err) {
				return receipt, fmt.Errorf("line: close disabled host session: %w", err)
			}
		} else if reply.Fields["absent"] != "1" {
			receipt.Applied = appendScopeConnection(receipt.Applied, *host)
		}
	}
	receipt.Absent = len(receipt.Applied) == 0
	return receipt, nil
}

// currentConnections drops frozen references that belong to a previous broker
// epoch. Connection ids are scoped to one broker process, so a delayed
// enforcement must never target the same numeric id in a later epoch.
func (m *Manager) currentConnections(frozen []store.ScopeConnection) []store.ScopeConnection {
	epoch := m.presence.BrokerEpoch()
	out := make([]store.ScopeConnection, 0, len(frozen))
	seen := map[store.ScopeConnection]struct{}{}
	for _, connection := range frozen {
		if !connection.Valid() {
			continue
		}
		if epoch != "" && connection.Ref.Epoch != epoch {
			continue
		}
		if _, ok := seen[connection]; ok {
			continue
		}
		seen[connection] = struct{}{}
		out = append(out, connection)
	}
	sort.Slice(out, func(i, j int) bool {
		if out[i].ClientID != out[j].ClientID {
			return out[i].ClientID < out[j].ClientID
		}
		return out[i].Ref.ID < out[j].Ref.ID
	})
	return out
}

func appendScopeConnection(list []store.ScopeConnection, connection store.ScopeConnection) []store.ScopeConnection {
	for _, existing := range list {
		if existing == connection {
			return list
		}
	}
	return append(list, connection)
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
