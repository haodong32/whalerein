package broker

import (
	"context"
	"encoding/json"
	"errors"
	"log/slog"
	"strings"
	"sync"
	"time"

	"github.com/whalerein/whalerein/access/internal/diagnostics"
)

type presenceEntry struct {
	online bool
	at     time.Time
}

// Presence is the broker-derived online registry. It is seeded only by EMQX
// $SYS client lifecycle messages, the EMQX presence webhook, or a host Ready
// signal. It never queries a database heartbeat.
type Presence struct {
	mu       sync.RWMutex
	hosts    map[string]presenceEntry
	devices  map[string]presenceEntry
	cacheTTL time.Duration
	rest     *RESTClient
	metrics  *diagnostics.Metrics
	logger   *slog.Logger
	now      func() time.Time
}

// NewPresence creates an empty broker presence cache.
func NewPresence(cacheTTL time.Duration, rest *RESTClient, metrics *diagnostics.Metrics, logger *slog.Logger) *Presence {
	if cacheTTL <= 0 {
		cacheTTL = 15 * time.Second
	}
	if logger == nil {
		logger = slog.Default()
	}
	p := &Presence{
		hosts:    make(map[string]presenceEntry),
		devices:  make(map[string]presenceEntry),
		cacheTTL: cacheTTL,
		rest:     rest,
		metrics:  metrics,
		logger:   logger,
		now:      func() time.Time { return time.Now().UTC() },
	}
	p.publishCounts()
	return p
}

// IsHostOnline returns broker presence. A fresh cache entry wins; otherwise the
// optional EMQX REST query refreshes it. Stale values are used only when the
// REST client is unavailable, so a broker outage cannot invent an online host.
func (p *Presence) IsHostOnline(ctx context.Context, hostID string) bool {
	if hostID == "" {
		return false
	}
	p.mu.RLock()
	entry, ok := p.hosts[hostID]
	p.mu.RUnlock()
	now := p.now()
	if ok && now.Sub(entry.at) < p.cacheTTL {
		return entry.online
	}
	if p.rest != nil && p.rest.Enabled() {
		online, err := p.rest.ClientConnected(ctx, ClientIDForHost(hostID))
		if err == nil {
			p.SetHostOnline(hostID, online, now)
			return online
		}
		p.logger.Debug("EMQX presence query failed", "error", err.Error())
	}
	return entry.online
}

// SetHostOnline updates broker-derived host presence.
func (p *Presence) SetHostOnline(hostID string, online bool, at time.Time) {
	if hostID == "" {
		return
	}
	p.mu.Lock()
	if previous, exists := p.hosts[hostID]; !exists || !at.Before(previous.at) {
		p.hosts[hostID] = presenceEntry{online: online, at: at}
	}
	p.mu.Unlock()
	p.publishCounts()
}

// SetDeviceOnline updates broker-derived device presence.
func (p *Presence) SetDeviceOnline(deviceID string, online bool, at time.Time) {
	if deviceID == "" {
		return
	}
	p.mu.Lock()
	if previous, exists := p.devices[deviceID]; !exists || !at.Before(previous.at) {
		p.devices[deviceID] = presenceEntry{online: online, at: at}
	}
	p.mu.Unlock()
	p.publishCounts()
}

// MarkHostReady treats a ready host result as broker presence.
func (p *Presence) MarkHostReady(hostID string, at time.Time) {
	p.SetHostOnline(hostID, true, at)
}

// HandleSysMessage consumes an EMQX $SYS client lifecycle message.
func (p *Presence) HandleSysMessage(topic string, payload []byte) {
	clientID, online, ok := ParseSysClientTopic(topic)
	if !ok {
		return
	}
	identity := identityForClient(clientID, "")
	if parsed := parsePresencePayload(payload); parsed.ClientID != "" {
		identity = identityForClient(parsed.ClientID, parsed.Username)
	} else if parsed.Username != "" {
		identity = identityForClient("", parsed.Username)
	}
	p.apply(identity, online, p.now())
}

// HandlePresenceWebhook consumes an EMQX client lifecycle webhook payload. An
// object or an array of objects is accepted.
func (p *Presence) HandlePresenceWebhook(body []byte) error {
	trimmed := strings.TrimSpace(string(body))
	if trimmed == "" {
		return errors.New("empty presence payload")
	}
	if strings.HasPrefix(trimmed, "[") {
		var events []presencePayload
		if err := json.Unmarshal(body, &events); err != nil {
			return err
		}
		for _, event := range events {
			p.applyPayload(event)
		}
		return nil
	}
	var event presencePayload
	if err := json.Unmarshal(body, &event); err != nil {
		return err
	}
	p.applyPayload(event)
	return nil
}

type presencePayload struct {
	Event     string `json:"event"`
	ClientID  string `json:"clientid"`
	Username  string `json:"username"`
	Timestamp int64  `json:"timestamp"`
}

func parsePresencePayload(payload []byte) presencePayload {
	var event presencePayload
	_ = json.Unmarshal(payload, &event)
	return event
}

func (p *Presence) applyPayload(event presencePayload) {
	identity := identityForClient(event.ClientID, event.Username)
	if identity.kind == "" {
		return
	}
	online, ok := lifecycleState(event.Event)
	if !ok {
		return
	}
	at := p.now()
	if event.Timestamp > 0 {
		at = time.UnixMilli(event.Timestamp).UTC()
	}
	p.apply(identity, online, at)
}

func lifecycleState(event string) (bool, bool) {
	name := strings.ToLower(event)
	switch {
	case strings.Contains(name, "disconnected"):
		return false, true
	case strings.Contains(name, "connected"):
		return true, true
	default:
		return false, false
	}
}

type clientIdentity struct {
	kind     string
	hostID   string
	deviceID string
}

func identityForClient(clientID, username string) clientIdentity {
	// Probe sessions must never become host/device presence, even when a
	// lifecycle callback omits either the username or the client ID.
	if strings.HasPrefix(username, "probe:") || strings.HasPrefix(clientID, "whalerein-probe-") {
		return clientIdentity{}
	}
	if strings.HasPrefix(username, "host:") {
		return clientIdentity{kind: "host", hostID: strings.TrimPrefix(username, "host:")}
	}
	if strings.HasPrefix(username, "device:") {
		return clientIdentity{kind: "device", deviceID: strings.TrimPrefix(username, "device:")}
	}
	if strings.HasPrefix(clientID, "whalerein-host-") {
		return clientIdentity{kind: "host", hostID: strings.TrimPrefix(clientID, "whalerein-host-")}
	}
	if strings.HasPrefix(clientID, "whalerein-device-") {
		return clientIdentity{kind: "device", deviceID: strings.TrimPrefix(clientID, "whalerein-device-")}
	}
	return clientIdentity{}
}

func (p *Presence) apply(identity clientIdentity, online bool, at time.Time) {
	switch identity.kind {
	case "host":
		p.SetHostOnline(identity.hostID, online, at)
	case "device":
		p.SetDeviceOnline(identity.deviceID, online, at)
	}
}

// ConnectedCounts returns the last broker-derived online counts.
func (p *Presence) ConnectedCounts() (hosts, devices int) {
	p.mu.RLock()
	defer p.mu.RUnlock()
	for _, entry := range p.hosts {
		if entry.online {
			hosts++
		}
	}
	for _, entry := range p.devices {
		if entry.online {
			devices++
		}
	}
	return hosts, devices
}

func (p *Presence) publishCounts() {
	if p.metrics == nil {
		return
	}
	hosts, devices := p.ConnectedCounts()
	p.metrics.SetConnected(hosts, devices)
}
