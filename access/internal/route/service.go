// Package route forwards a Host's signed route attachment to Central.
//
// Access is the node that observed the authenticated Host transport. It fills
// its own node identity, verifies that the Host signed the exact tuple with its
// enrolled keys, and forwards the proof. Central remains the route authority.
package route

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"sync"
	"time"

	"github.com/whalerein/whalerein/access/internal/control"
	"github.com/whalerein/whalerein/access/internal/routeproof"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/encoding/protojson"
)

// EnrollmentReader resolves the enrolled Host keys for a ready Host.
type EnrollmentReader interface {
	Enrollment(context.Context, string) (store.ExternalEnrollment, error)
}

// PendingStore is the durable retry seam.
type PendingStore interface {
	SavePendingRoute(context.Context, store.PendingRoute) error
	DeletePendingRoute(context.Context, string, uint64) error
	ListPendingRoutes(context.Context) ([]store.PendingRoute, error)
}

// Service verifies and forwards route attachments.
type Service struct {
	Enrollments EnrollmentReader
	Control     *control.Client
	NodeID      string
	Incarnation string
	Now         func() time.Time
	// Observe records one route control attempt. It must not block.
	Observe func(operation string, ok bool)
	// Pending persists retries across a process restart. It is optional.
	Pending PendingStore

	mu              sync.Mutex
	pending         map[string]*whalereinv1.HostRoute
	pendingAttaches map[string]*whalereinv1.HostRouteAttach
	routes          map[string]*whalereinv1.HostRoute
}

// Attach validates one HostReady route proof. A Host without a route proof is
// left alone: old clients may still connect, but they get no managed route.
func (s *Service) Attach(ctx context.Context, hostID string, ready *whalereinv1.HostReady) error {
	if s.Control == nil {
		return nil
	}
	if ready.GetRouteGeneration() == 0 {
		return nil
	}
	if ready.GetHostId() != hostID {
		return errors.New("route: HostReady host identity changed")
	}
	if s.Enrollments == nil {
		return errors.New("route: enrollment store is unavailable")
	}
	enrollment, err := s.Enrollments.Enrollment(ctx, hostID)
	if err != nil {
		return fmt.Errorf("route: enrollment: %w", err)
	}
	if enrollment.DisabledAt != nil {
		return errors.New("route: enrollment is disabled")
	}
	if !bytes.Equal(enrollment.HostSigningPublicKey, ready.GetRouteSigningPublicKey()) ||
		!bytes.Equal(enrollment.HostNoisePublicKey, ready.GetRouteNoisePublicKey()) {
		return errors.New("route: announced keys do not match the enrollment")
	}
	service, environment := ready.GetRouteService(), ready.GetRouteEnvironment()
	if service == "" || environment == "" {
		return errors.New("route: service and environment are required")
	}
	signedAt := time.UnixMilli(ready.GetRouteSignedAtUnixMs()).UTC()
	now := time.Now().UTC()
	if s.Now != nil {
		now = s.Now().UTC()
	}
	// Bound clock error without extending the signature indefinitely.
	if signedAt.After(now.Add(2*time.Minute)) || signedAt.Before(now.Add(-10*time.Minute)) {
		return errors.New("route: attach signature is outside the accepted clock window")
	}
	if !routeproof.Verify(enrollment.HostSigningPublicKey, service, environment, hostID,
		s.NodeID, s.Incarnation, ready.GetRouteChallenge(), ready.GetRouteGeneration(),
		enrollment.HostNoisePublicKey, signedAt, ready.GetRouteSignature()) {
		return errors.New("route: attach signature is invalid")
	}
	attach := &whalereinv1.HostRouteAttach{
		HostId: hostID, WorkspaceId: enrollment.WorkspaceID,
		Service: service, Environment: environment,
		NodeId: s.NodeID, Incarnation: s.Incarnation,
		Generation: ready.GetRouteGeneration(), Challenge: ready.GetRouteChallenge(),
		SignedAtUnixMs: signedAt.UnixMilli(), HostSignature: ready.GetRouteSignature(),
		HostSigningPublicKey: enrollment.HostSigningPublicKey,
		HostNoisePublicKey:   enrollment.HostNoisePublicKey,
		NodeObserved:         true, NodeIncarnation: s.Incarnation,
	}
	if err := s.Control.Attach(ctx, attach); err != nil {
		// A Host can publish HostReady before the first heartbeat bootstrap
		// reaches Central. Registration is idempotent for this incarnation, so
		// make one bootstrap attempt and retry the exact attach.
		if registerErr := s.Control.Register(ctx); registerErr == nil {
			if retryErr := s.Control.Attach(ctx, attach); retryErr != nil {
				s.queueAttach(attach)
				s.observe("attach", false)
				return retryErr
			}
			s.clearAttached(attach)
			s.rememberRoute(attach)
			s.observe("attach", true)
			return nil
		}
		s.queueAttach(attach)
		s.observe("attach", false)
		return err
	}
	s.clearAttached(attach)
	s.rememberRoute(attach)
	s.observe("attach", true)
	return nil
}

func (s *Service) rememberRoute(attach *whalereinv1.HostRouteAttach) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.routes == nil {
		s.routes = map[string]*whalereinv1.HostRoute{}
	}
	s.routes[attach.GetHostId()] = &whalereinv1.HostRoute{
		HostId: attach.GetHostId(), WorkspaceId: attach.GetWorkspaceId(),
		NodeId: s.NodeID, Incarnation: s.Incarnation, Generation: attach.GetGeneration(),
		State: whalereinv1.RouteState_ROUTE_OPEN, Challenge: attach.GetChallenge(),
	}
}

// Route returns the current open route observed for one Host.
func (s *Service) Route(hostID string) (*whalereinv1.HostRoute, bool) {
	if s == nil {
		return nil, false
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	route, ok := s.routes[hostID]
	return route, ok
}

func (s *Service) clearAttached(attach *whalereinv1.HostRouteAttach) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if current := s.pendingAttaches[attach.GetHostId()]; current != nil && current.GetGeneration() <= attach.GetGeneration() {
		delete(s.pendingAttaches, attach.GetHostId())
	}
	if close := s.pending[attach.GetHostId()]; close != nil && close.GetGeneration() <= attach.GetGeneration() {
		delete(s.pending, attach.GetHostId())
	}
	if s.Pending != nil {
		_ = s.Pending.DeletePendingRoute(context.Background(), attach.GetHostId(), attach.GetGeneration())
	}
}

func (s *Service) observe(operation string, ok bool) {
	if s.Observe != nil {
		s.Observe(operation, ok)
	}
}

// Close forwards the exact open tuple when a Host shuts down gracefully. The
// same attach proof is still verified so an arbitrary HostReady cannot close
// another Host's route; Central accepts a same-tuple close from the serving
// node without a second signature.
func (s *Service) Close(ctx context.Context, hostID string, ready *whalereinv1.HostReady) error {
	if s.Control == nil || ready.GetRouteGeneration() == 0 {
		return nil
	}
	if ready.GetHostId() != hostID {
		return errors.New("route: HostReady host identity changed")
	}
	if s.Enrollments == nil {
		return errors.New("route: enrollment store is unavailable")
	}
	enrollment, err := s.Enrollments.Enrollment(ctx, hostID)
	if err != nil {
		return fmt.Errorf("route: enrollment: %w", err)
	}
	if !bytes.Equal(enrollment.HostSigningPublicKey, ready.GetRouteSigningPublicKey()) ||
		!bytes.Equal(enrollment.HostNoisePublicKey, ready.GetRouteNoisePublicKey()) {
		return errors.New("route: announced keys do not match the enrollment")
	}
	signedAt := time.UnixMilli(ready.GetRouteSignedAtUnixMs()).UTC()
	if !routeproof.Verify(enrollment.HostSigningPublicKey, ready.GetRouteService(), ready.GetRouteEnvironment(),
		hostID, s.NodeID, s.Incarnation, ready.GetRouteChallenge(), ready.GetRouteGeneration(),
		enrollment.HostNoisePublicKey, signedAt, ready.GetRouteSignature()) {
		return errors.New("route: attach signature is invalid")
	}
	close := &whalereinv1.HostRouteClose{
		HostId: hostID, NodeId: s.NodeID, Incarnation: s.Incarnation,
		Generation: ready.GetRouteGeneration(), Challenge: ready.GetRouteChallenge(),
		Service: ready.GetRouteService(), Environment: ready.GetRouteEnvironment(),
	}
	if err := s.Control.Close(ctx, close); err != nil {
		// Central may be restarting. Keep the exact terminal tuple so the next
		// inventory can close it without reopening or minting a new route.
		s.queueClose(ready, hostID)
		s.observe("close", false)
		return err
	}
	s.clearClosed(hostID, ready.GetRouteGeneration())
	s.observe("close", true)
	return nil
}

func (s *Service) clearClosed(hostID string, generation uint64) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if current := s.pending[hostID]; current != nil && current.GetGeneration() <= generation {
		delete(s.pending, hostID)
	}
	delete(s.routes, hostID)
	if attach := s.pendingAttaches[hostID]; attach != nil && attach.GetGeneration() <= generation {
		delete(s.pendingAttaches, hostID)
	}
	if s.Pending != nil {
		_ = s.Pending.DeletePendingRoute(context.Background(), hostID, generation)
	}
}

func (s *Service) queueAttach(attach *whalereinv1.HostRouteAttach) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.pendingAttaches == nil {
		s.pendingAttaches = map[string]*whalereinv1.HostRouteAttach{}
	}
	s.pendingAttaches[attach.GetHostId()] = attach
	if s.Pending != nil {
		if raw, err := protojson.Marshal(attach); err == nil {
			_ = s.Pending.SavePendingRoute(context.Background(), store.PendingRoute{
				HostID: attach.GetHostId(), Kind: store.PendingAttach,
				Generation: attach.GetGeneration(), Payload: raw,
			})
		}
	}
}

func (s *Service) queueClose(ready *whalereinv1.HostReady, hostID string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.pending == nil {
		s.pending = map[string]*whalereinv1.HostRoute{}
	}
	route := &whalereinv1.HostRoute{
		HostId: hostID, NodeId: s.NodeID, Incarnation: s.Incarnation,
		Generation: ready.GetRouteGeneration(), State: whalereinv1.RouteState_ROUTE_CLOSED,
		Challenge: ready.GetRouteChallenge(), UpdatedAtUnixMs: time.Now().UTC().UnixMilli(),
	}
	s.pending[hostID] = route
	if s.Pending != nil {
		if raw, err := protojson.Marshal(route); err == nil {
			_ = s.Pending.SavePendingRoute(context.Background(), store.PendingRoute{
				HostID: hostID, Kind: store.PendingClose,
				Generation: route.GetGeneration(), Payload: raw,
			})
		}
	}
}

// PendingCount reports queued retries for bounded operator metrics.
func (s *Service) PendingCount() int {
	if s == nil {
		return 0
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	return len(s.pending) + len(s.pendingAttaches)
}

// Restore loads the durable retry queue after a process restart.
func (s *Service) Restore(ctx context.Context) error {
	if s.Pending == nil {
		return nil
	}
	routes, err := s.Pending.ListPendingRoutes(ctx)
	if err != nil {
		return err
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	for _, route := range routes {
		switch route.Kind {
		case store.PendingAttach:
			value := &whalereinv1.HostRouteAttach{}
			if err := protojson.Unmarshal(route.Payload, value); err == nil {
				if s.pendingAttaches == nil {
					s.pendingAttaches = map[string]*whalereinv1.HostRouteAttach{}
				}
				s.pendingAttaches[route.HostID] = value
			}
		case store.PendingClose:
			value := &whalereinv1.HostRoute{}
			if err := protojson.Unmarshal(route.Payload, value); err == nil {
				if s.pending == nil {
					s.pending = map[string]*whalereinv1.HostRoute{}
				}
				s.pending[route.HostID] = value
			}
		}
	}
	return nil
}

// Reconcile retries queued attaches and terminal routes. It is safe to call
// periodically; successful entries are removed by host/generation.
func (s *Service) Reconcile(ctx context.Context) error {
	if s.Control == nil {
		return nil
	}
	s.mu.Lock()
	attaches := make([]*whalereinv1.HostRouteAttach, 0, len(s.pendingAttaches))
	for _, attach := range s.pendingAttaches {
		attaches = append(attaches, attach)
	}
	s.mu.Unlock()
	var errs []error
	for _, attach := range attaches {
		if err := s.Control.Attach(ctx, attach); err != nil {
			errs = append(errs, err)
			continue
		}
		s.mu.Lock()
		if current := s.pendingAttaches[attach.GetHostId()]; current != nil && current.GetGeneration() == attach.GetGeneration() {
			delete(s.pendingAttaches, attach.GetHostId())
		}
		if s.Pending != nil {
			_ = s.Pending.DeletePendingRoute(context.Background(), attach.GetHostId(), attach.GetGeneration())
		}
		// A newer attach supersedes an older queued close.
		if close := s.pending[attach.GetHostId()]; close != nil && close.GetGeneration() <= attach.GetGeneration() {
			delete(s.pending, attach.GetHostId())
		}
		s.mu.Unlock()
	}
	s.mu.Lock()
	routes := make([]*whalereinv1.HostRoute, 0, len(s.pending))
	for _, route := range s.pending {
		routes = append(routes, route)
	}
	s.mu.Unlock()
	if len(routes) > 0 {
		if _, err := s.Control.Inventory(ctx, s.NodeID, s.Incarnation, routes); err != nil {
			errs = append(errs, err)
		} else {
			s.mu.Lock()
			for _, route := range routes {
				if current := s.pending[route.GetHostId()]; current != nil && current.GetGeneration() == route.GetGeneration() {
					delete(s.pending, route.GetHostId())
				}
				if s.Pending != nil {
					_ = s.Pending.DeletePendingRoute(context.Background(), route.GetHostId(), route.GetGeneration())
				}
			}
			s.mu.Unlock()
		}
	}
	return errors.Join(errs...)
}

// ReconcileLoop retries terminal route reconciliation until the context ends.
func (s *Service) ReconcileLoop(ctx context.Context, interval time.Duration) {
	if interval <= 0 {
		interval = 10 * time.Second
	}
	ticker := time.NewTicker(interval)
	defer ticker.Stop()
	for {
		_ = s.Reconcile(ctx)
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
		}
	}
}
