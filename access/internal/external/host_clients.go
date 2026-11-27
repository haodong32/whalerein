package external

import (
	"context"
	"crypto/ed25519"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

type HostClientGrants interface {
	HostClientIDs(ctx context.Context, workspaceID, hostID, revision string) ([]string, error)
}

type HostClientProbe interface {
	PublishAndAwait(context.Context, string, *whalereinv1.HostRequest, time.Duration) (*whalereinv1.HostResult, error)
}

type HostPresence interface {
	HostPresence(hostID string) (online, known bool)
}

func WithHostClients(grants HostClientGrants, probe HostClientProbe, presence HostPresence) Option {
	return func(s *Service) {
		s.hostClientGrants = grants
		s.hostClientProbe = probe
		s.hostPresence = presence
	}
}

type HostClient struct {
	DeviceID  string `json:"deviceId"`
	Connected bool   `json:"connected"`
}

type HostClientList struct {
	Presence string       `json:"presence"`
	Devices  []HostClient `json:"devices"`
}

// HostClients combines verified pairing grants with an on-demand observation
// from the exact Host. The response never contains Session or channel IDs.
func (s *Service) HostClients(ctx context.Context, workspaceID, hostID, revision string) (HostClientList, error) {
	if s.hostClientGrants == nil {
		return HostClientList{}, ErrEnforcementUnavailable
	}
	ids, err := s.hostClientGrants.HostClientIDs(ctx, workspaceID, hostID, revision)
	if err != nil {
		return HostClientList{}, err
	}
	list := HostClientList{Presence: "unavailable", Devices: make([]HostClient, 0, len(ids))}
	for _, id := range ids {
		list.Devices = append(list.Devices, HostClient{DeviceID: id})
	}
	if len(ids) == 0 {
		list.Presence = "known"
		return list, nil
	}
	if s.hostPresence == nil {
		return list, nil
	}
	online, known := s.hostPresence.HostPresence(hostID)
	if !known {
		return list, nil
	}
	if !online {
		list.Presence = "known"
		return list, nil
	}
	if s.hostClientProbe == nil {
		return list, nil
	}
	observing, ok := s.observeHostClients(ctx, hostID)
	if !ok {
		return list, nil
	}
	list.Presence = "known"
	for i := range list.Devices {
		_, list.Devices[i].Connected = observing[list.Devices[i].DeviceID]
	}
	return list, nil
}

func (s *Service) observeHostClients(ctx context.Context, hostID string) (map[string]struct{}, bool) {
	const timeout = time.Second
	request := &whalereinv1.HostRequest{
		RequestId:       store.NewOperationID(),
		HostId:          hostID,
		Kind:            whalereinv1.RequestKind_REQUEST_KIND_HOST_CLIENTS,
		ProtocolVersion: 1,
		ExpiresAtUnixMs: s.now().Add(timeout).UnixMilli(),
	}
	probeCtx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()
	result, err := s.hostClientProbe.PublishAndAwait(probeCtx, hostID, request, timeout)
	if err != nil || result.GetRequestId() != request.RequestId || result.GetHostId() != hostID ||
		result.GetStatus() != whalereinv1.OnlineStatus_succeeded {
		return nil, false
	}
	var clients whalereinv1.HostClients
	if err := proto.Unmarshal(result.GetResultBody(), &clients); err != nil || len(clients.ObservingDeviceIds) > 512 {
		return nil, false
	}
	observing := make(map[string]struct{}, len(clients.ObservingDeviceIds))
	for _, id := range clients.ObservingDeviceIds {
		observing[id] = struct{}{}
	}
	return observing, true
}

// RegisterSessionProof verifies that the bearer names the exact live Device
// session, then relays the transcript-bound possession proof to the Host.
func (s *Service) RegisterSessionProof(ctx context.Context, sessionID, credential string, proof *whalereinv1.SessionProof) error {
	if s.hostClientProbe == nil || proof == nil || credential == "" {
		return ErrEnforcementUnavailable
	}
	if s.deviceSessions == nil {
		return ErrEnforcementUnavailable
	}
	now := s.now()
	issued, expires := proof.GetIssuedAtUnixMs(), proof.GetExpiresAtUnixMs()
	if issued <= 0 || expires <= now.UnixMilli() || expires-issued > 60_000 ||
		issued > now.Add(5*time.Second).UnixMilli() {
		return ErrAuthorizationMismatch
	}
	permission, err := s.deviceSessions.DeviceSessionByCredential(ctx, HashBytes([]byte(credential)), now)
	if err != nil || permission.ID != sessionID || permission.DeviceID != proof.GetDeviceId() ||
		len(permission.ProofPublicKey) != ed25519.PublicKeySize {
		return ErrAuthorizationMismatch
	}
	s.proofMu.Lock()
	if s.sessionProofs == nil {
		s.sessionProofs = map[string]*whalereinv1.SessionProof{}
	}
	s.sessionProofs[proof.GetSessionId()] = proto.Clone(proof).(*whalereinv1.SessionProof)
	s.proofMu.Unlock()
	return s.relaySessionProof(ctx, proof)
}

func (s *Service) relaySessionProof(ctx context.Context, proof *whalereinv1.SessionProof) error {
	body, err := proto.Marshal(proof)
	if err != nil {
		return err
	}
	return s.publishHostRequest(ctx, proof.GetHostId(),
		whalereinv1.RequestKind_REQUEST_KIND_SESSION_PROOF,
		proof.GetDeviceId(), proof.GetChannelId(), body)
}

// publishHostRequest is the single Access -> Host request/response shape for
// authority and proof delivery. The caller supplies an already-encoded body.
func (s *Service) publishHostRequest(
	ctx context.Context, hostID string, kind whalereinv1.RequestKind,
	deviceID, channelID string, body []byte,
) error {
	const timeout = 2 * time.Second
	request := &whalereinv1.HostRequest{
		RequestId:       store.NewOperationID(),
		HostId:          hostID,
		DeviceId:        deviceID,
		Kind:            kind,
		ProtocolVersion: 1,
		ChannelId:       channelID,
		ExpiresAtUnixMs: s.now().Add(timeout).UnixMilli(),
		Body:            body,
	}
	probeCtx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()
	result, err := s.hostClientProbe.PublishAndAwait(probeCtx, hostID, request, timeout)
	if err != nil {
		return err
	}
	if result.GetRequestId() != request.RequestId || result.GetHostId() != hostID ||
		result.GetStatus() != whalereinv1.OnlineStatus_succeeded {
		return ErrEnforcementUnavailable
	}
	return nil
}

// PublishAuthority sends one Central-signed App-session permit to the Host and
// waits for its independent acceptance. Access never marks a permit delivered
// on a lost or rejected response.
func (s *Service) PublishAuthority(ctx context.Context, hostID string, authority *whalereinv1.ScopedAuthority) error {
	if s.hostClientProbe == nil || authority == nil {
		return ErrEnforcementUnavailable
	}
	body, err := proto.Marshal(authority)
	if err != nil {
		return err
	}
	if err := s.publishHostRequest(ctx, hostID,
		whalereinv1.RequestKind_REQUEST_KIND_AUTHORITY,
		authority.GetDeviceId(), "", body); err != nil {
		return err
	}
	// A proof can arrive before its permit. Retry the stored proof now that the
	// Host has staged the authority.
	s.proofMu.Lock()
	proof := s.sessionProofs[authority.GetSessionId()]
	s.proofMu.Unlock()
	if proof != nil {
		_ = s.relaySessionProof(ctx, proof)
	}
	return nil
}
