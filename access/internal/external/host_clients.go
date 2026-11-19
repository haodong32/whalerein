package external

import (
	"context"
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
