package auth

import (
	"context"
	"crypto/rand"
	"encoding/base64"
	"errors"
	"fmt"
	"time"

	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

// RevokeDevice is the only revocation entry point. The database transaction
// commits grants.revoked_at first (ACLs are database-derived), then Access
// disconnects the device and tells each online host. replayed_at is written
// only for hosts that acknowledged the notice.
func (s *Service) RevokeDevice(ctx context.Context, deviceID string) error {
	if deviceID == "" {
		return ErrBadRequest
	}
	at := s.now()
	// Fence every realtime admission before the durable write. A later
	// confirmed per-grant denial narrows the provisional device fence.
	s.admission.DenyDevice(deviceID)
	hostIDs, exists, err := s.store.RevokeDeviceGrants(ctx, deviceID, at)
	if err != nil {
		return fmt.Errorf("revoke device: %w", err)
	}
	if !exists {
		return ErrNotFound
	}
	for _, hostID := range hostIDs {
		s.admission.Deny(hostID, deviceID)
	}
	s.admission.CompleteDeviceRevoke(deviceID)
	if err := s.enforcer.RevokeDevice(ctx, deviceID); err != nil {
		return fmt.Errorf("revoke device policy: %w", err)
	}

	s.logger.Info("revocation committed", "source", "device_request", "trace_id", diagnostics.TraceID(ctx), "device", s.masker.Device(deviceID), "count", len(hostIDs), "revoked_at_unix_ms", at.UnixMilli())
	if err := s.transport.DisconnectDevice(ctx, deviceID); err != nil {
		s.logger.Warn("revocation disconnect failed",
			"device", s.masker.Device(deviceID), "error", err.Error())
	}

	for _, hostID := range hostIDs {
		if !s.presence.IsHostOnline(ctx, hostID) {
			continue
		}
		if err := s.notifyRevokedGrant(ctx, hostID, deviceID); err != nil {
			return err
		}
	}
	return nil
}

// notifyRevokedGrant snapshots an incarnation after disconnect and preserves it in the notice.
func (s *Service) notifyRevokedGrant(ctx context.Context, hostID, deviceID string) error {
	pending, err := s.store.PendingRevocations(ctx, hostID)
	if err != nil {
		return fmt.Errorf("read revoked grant: %w", err)
	}
	var grant *store.Revocation
	for index := range pending {
		if pending[index].DeviceID == deviceID {
			grant = &pending[index]
			break
		}
	}
	if grant == nil {
		return nil
	}
	started := s.now()
	notice := &whalereinv1.RevocationNotice{
		ReplayId:         opaqueID(),
		DeviceIds:        []string{deviceID},
		Final:            false,
		RevokedAtUnixMs:  grant.RevokedAt.UnixMilli(),
		GrantGenerations: map[string]uint64{deviceID: grant.GrantGeneration},
	}
	req, err := revocationRequest(hostID, notice, s.cfg.ProtocolVersion, s.now().Add(s.cfg.RevocationAckTimeout))
	if err != nil {
		s.logger.Warn("revocation encode failed",
			"host", s.masker.Host(hostID), "device", s.masker.Device(deviceID), "error", err.Error())
		return nil
	}
	result, err := s.transport.PublishAndAwait(ctx, hostID, req, s.cfg.RevocationAckTimeout)
	if err != nil {
		s.logger.Warn("revocation notice not acknowledged",
			"host", s.masker.Host(hostID), "device", s.masker.Device(deviceID), "error", err.Error())
		return nil
	}
	if err := verifyRevocationAck(result, notice); err != nil {
		s.logger.Warn("revocation acknowledgement rejected",
			"host", s.masker.Host(hostID), "device", s.masker.Device(deviceID), "error", err.Error())
		return nil
	}
	if err := s.store.MarkRevocationsReplayed(ctx, hostID, notice.DeviceIds, grant.RevokedAt, s.now(), notice.GrantGenerations); err != nil {
		return fmt.Errorf("mark revocation replayed: %w", err)
	}
	s.metrics.ObserveRevocation(s.now().Sub(started))
	s.logger.Info("device revoked",
		"host", s.masker.Host(hostID), "device", s.masker.Device(deviceID))
	return nil
}

// ReplayPendingRevocations runs when a host signals readiness. It publishes
// every pending revocation, waits for the acknowledgement, then publishes a
// final empty notice. The host rejects new channels until it receives
// RevocationNotice{final=true}.
func (s *Service) ReplayPendingRevocations(ctx context.Context, hostID string) error {
	if hostID == "" {
		return ErrBadRequest
	}
	pending, err := s.store.PendingRevocations(ctx, hostID)
	if err != nil {
		return fmt.Errorf("replay revocations: %w", err)
	}
	now := s.now()
	if len(pending) > 0 {
		deviceIDs := make([]string, 0, len(pending))
		generations := make(map[string]uint64, len(pending))
		var oldest time.Time
		var newest time.Time
		for _, item := range pending {
			deviceIDs = append(deviceIDs, item.DeviceID)
			generations[item.DeviceID] = item.GrantGeneration
			if oldest.IsZero() || item.RevokedAt.Before(oldest) {
				oldest = item.RevokedAt
			}
			if item.RevokedAt.After(newest) {
				newest = item.RevokedAt
			}
		}
		notice := &whalereinv1.RevocationNotice{
			ReplayId:         opaqueID(),
			DeviceIds:        deviceIDs,
			Final:            false,
			RevokedAtUnixMs:  newest.UnixMilli(),
			GrantGenerations: generations,
		}
		req, err := revocationRequest(hostID, notice, s.cfg.ProtocolVersion, now.Add(s.cfg.RevocationAckTimeout))
		if err != nil {
			return err
		}
		result, err := s.transport.PublishAndAwait(ctx, hostID, req, s.cfg.RevocationAckTimeout)
		if err != nil {
			return fmt.Errorf("replay revocation notice: %w", err)
		}
		if err := verifyRevocationAck(result, notice); err != nil {
			return fmt.Errorf("replay revocation ack: %w", err)
		}
		if err := s.store.MarkRevocationsReplayed(ctx, hostID, deviceIDs, newest, s.now(), generations); err != nil {
			return fmt.Errorf("mark replayed revocations: %w", err)
		}
		latency := s.now().Sub(oldest)
		if latency < 0 {
			latency = 0
		}
		s.metrics.ObserveRevocation(latency)
		s.logger.Info("revocation replay acknowledged",
			"host", s.masker.Host(hostID), "count", len(deviceIDs), "source", "pending_replay", "revoked_at_unix_ms", newest.UnixMilli(), "oldest_revoked_at_unix_ms", oldest.UnixMilli())
	}

	final := &whalereinv1.RevocationNotice{
		ReplayId:        opaqueID(),
		DeviceIds:       nil,
		Final:           true,
		RevokedAtUnixMs: s.now().UnixMilli(),
	}
	req, err := revocationRequest(hostID, final, s.cfg.ProtocolVersion, s.now().Add(s.cfg.RevocationAckTimeout))
	if err != nil {
		return err
	}
	// The host acknowledges the final batch too. Awaiting it both prevents a
	// stray result from being routed as an unknown request and confirms that
	// the host cleared revocations_pending.
	finalResult, err := s.transport.PublishAndAwait(ctx, hostID, req, s.cfg.RevocationAckTimeout)
	if err != nil {
		return fmt.Errorf("publish final revocation notice: %w", err)
	}
	if err := verifyRevocationAck(finalResult, final); err != nil {
		return fmt.Errorf("final revocation ack: %w", err)
	}
	s.logger.Info("host readiness handled", "host", s.masker.Host(hostID))
	return nil
}

func revocationRequest(hostID string, notice *whalereinv1.RevocationNotice, protocolVersion uint32, expires time.Time) (*whalereinv1.HostRequest, error) {
	body, err := proto.Marshal(notice)
	if err != nil {
		return nil, fmt.Errorf("marshal revocation notice: %w", err)
	}
	return &whalereinv1.HostRequest{
		RequestId:       opaqueID(),
		HostId:          hostID,
		Kind:            whalereinv1.RequestKind_REQUEST_KIND_REVOCATION,
		ExpiresAtUnixMs: expires.UnixMilli(),
		ProtocolVersion: protocolVersion,
		Body:            body,
	}, nil
}

func verifyRevocationAck(result *whalereinv1.HostResult, notice *whalereinv1.RevocationNotice) error {
	if result == nil {
		return errors.New("empty revocation result")
	}
	if result.GetStatus() != whalereinv1.OnlineStatus_succeeded {
		return fmt.Errorf("revocation status %s", store.StatusName(result.GetStatus()))
	}
	var ack whalereinv1.RevocationAck
	if err := proto.Unmarshal(result.GetResultBody(), &ack); err != nil {
		return fmt.Errorf("decode revocation ack: %w", err)
	}
	if ack.GetReplayId() != notice.GetReplayId() {
		return errors.New("revocation replay id mismatch")
	}
	acked := make(map[string]struct{}, len(ack.GetDeviceIds()))
	for _, id := range ack.GetDeviceIds() {
		acked[id] = struct{}{}
	}
	for _, id := range notice.GetDeviceIds() {
		if _, ok := acked[id]; !ok {
			return errors.New("revocation ack missing device")
		}
	}
	return nil
}

func opaqueID() string {
	raw := make([]byte, 18)
	if _, err := rand.Read(raw); err != nil {
		panic("auth: cannot read random id: " + err.Error())
	}
	return base64.RawURLEncoding.EncodeToString(raw)
}
