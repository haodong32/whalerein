// Package authority requests and caches Central-signed App-session permits on
// the serving Access node. It never signs or extends authority locally.
package authority

import (
	"context"
	"errors"
	"log/slog"
	"sync"
	"time"

	"github.com/whalerein/whalerein/access/internal/control"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// ScopeSource lists live sessions eligible for a permit.
type ScopeSource interface {
	AuthorityScopes(context.Context, time.Time) ([]store.AuthorityScope, error)
}

// Publisher delivers one verified permit to the Host.
type Publisher interface {
	PublishAuthority(context.Context, string, *whalereinv1.ScopedAuthority) error
}

// RouteSource returns the current open route for a Host.
type RouteSource interface {
	Route(string) (*whalereinv1.HostRoute, bool)
}

// Worker renews permits for current sessions and caches the last accepted
// permit. Delivery to Host is a separate step.
type Worker struct {
	Scopes      ScopeSource
	Routes      RouteSource
	Control     *control.Client
	Publisher   Publisher
	NodeID      string
	Incarnation string
	Environment string
	Interval    time.Duration
	Now         func() time.Time
	Logger      *slog.Logger

	mu          sync.Mutex
	authorities map[string]*whalereinv1.ScopedAuthority
}

// Renew requests one permit for every eligible session with a current route.
func (w *Worker) Renew(ctx context.Context) error {
	if w == nil || w.Scopes == nil || w.Control == nil {
		return nil
	}
	now := time.Now().UTC()
	if w.Now != nil {
		now = w.Now().UTC()
	}
	scopes, err := w.Scopes.AuthorityScopes(ctx, now)
	if err != nil {
		return err
	}
	var errs []error
	for _, scope := range scopes {
		if w.Routes == nil {
			continue
		}
		route, ok := w.Routes.Route(scope.HostID)
		if !ok || route.GetNodeId() != w.NodeID || route.GetIncarnation() != w.Incarnation {
			continue
		}
		permit, err := w.Control.Authority(ctx, &whalereinv1.AuthorityRenewalRequest{
			HostId: scope.HostID, DeviceId: scope.DeviceID, SessionId: scope.SessionID,
			GrantGeneration: scope.GrantGeneration, RouteGeneration: route.GetGeneration(),
			NodeId: w.NodeID, Incarnation: w.Incarnation, Environment: w.Environment,
			SessionProofPublicKey: scope.ProofPublicKey,
		})
		if err != nil {
			errs = append(errs, err)
			if w.Logger != nil {
				w.Logger.Warn("authority renewal failed", "host_id", scope.HostID,
					"session_id", scope.SessionID, "error", err.Error())
			}
			continue
		}
		w.mu.Lock()
		if w.authorities == nil {
			w.authorities = map[string]*whalereinv1.ScopedAuthority{}
		}
		w.authorities[scope.SessionID] = permit
		w.mu.Unlock()
		if w.Publisher != nil {
			if err := w.Publisher.PublishAuthority(ctx, scope.HostID, permit); err != nil {
				errs = append(errs, err)
				if w.Logger != nil {
					w.Logger.Warn("authority delivery failed", "host_id", scope.HostID,
						"session_id", scope.SessionID, "error", err.Error())
				}
			}
		}
	}
	return errors.Join(errs...)
}

// Authority returns the last accepted permit for one App session.
func (w *Worker) Authority(sessionID string) *whalereinv1.ScopedAuthority {
	if w == nil {
		return nil
	}
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.authorities[sessionID]
}

// Run renews immediately and then on the configured interval until cancel.
func (w *Worker) Run(ctx context.Context) {
	if w == nil {
		return
	}
	interval := w.Interval
	if interval <= 0 {
		interval = 20 * time.Second
	}
	ticker := time.NewTicker(interval)
	defer ticker.Stop()
	for {
		_ = w.Renew(ctx)
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
		}
	}
}
