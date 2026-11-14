package httpapi

import (
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"

	"github.com/whalerein/whalerein/access/internal/auth"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/policy"
	"github.com/whalerein/whalerein/access/internal/requests"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
)

func (s *Server) handleInvitation(w http.ResponseWriter, r *http.Request, _ map[string]string) {
	var request whalereinv1.PairingInvitationRequest
	if err := s.decodeProto(w, r, &request); err != nil {
		s.writeError(w, r, err)
		return
	}
	hostID := strings.TrimSpace(request.GetHostId())
	if hostID == "" {
		s.writeError(w, r, auth.ErrBadRequest)
		return
	}
	if token, err := bearerToken(r); err == nil {
		if adminErr := s.deps.Auth.AuthenticateAdmin(token); adminErr != nil {
			s.writeError(w, r, adminErr)
			return
		}
	} else if authErr := s.deps.Auth.AuthenticateHostRequest(r.Context(), auth.HostProof{
		Purpose:        auth.HostAuthInvitation,
		HostID:         hostID,
		SignedAtUnixMs: request.GetSignedAtUnixMs(),
		Signature:      request.GetSignature(),
	}); authErr != nil {
		s.writeError(w, r, authErr)
		return
	}
	invitation, err := s.deps.Pairing.CreateInvitation(r.Context(), hostID)
	if err != nil {
		s.writeError(w, r, err)
		return
	}
	s.writeProto(w, http.StatusOK, invitation)
}

func (s *Server) handleHostRevocation(w http.ResponseWriter, r *http.Request, params map[string]string) {
	hostID := params["host_id"]
	deviceID := params["device_id"]
	if !validResourceID(hostID) || !validResourceID(deviceID) {
		s.writeError(w, r, auth.ErrBadRequest)
		return
	}
	var request whalereinv1.HostGrantRevocation
	if err := s.decodeProto(w, r, &request); err != nil {
		s.writeError(w, r, err)
		return
	}
	if err := s.deps.Auth.AuthenticateHostRequest(r.Context(), auth.HostProof{
		Purpose:        auth.HostAuthRevoke,
		HostID:         hostID,
		DeviceID:       deviceID,
		SignedAtUnixMs: request.GetSignedAtUnixMs(),
		Signature:      request.GetSignature(),
	}); err != nil {
		s.writeError(w, r, err)
		return
	}
	if err := s.deps.Auth.RevokeHostGrant(r.Context(), hostID, deviceID); err != nil {
		s.writeError(w, r, err)
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

func validResourceID(value string) bool {
	return value != "" && !strings.ContainsAny(value, "/+#\x00")
}

func (s *Server) handleClaim(w http.ResponseWriter, r *http.Request, _ map[string]string) {
	var request whalereinv1.PairingClaimRequest
	if err := s.decodeProto(w, r, &request); err != nil {
		s.writeError(w, r, err)
		return
	}
	response, err := s.deps.Pairing.Claim(r.Context(), &request)
	if err != nil {
		s.writeError(w, r, err)
		return
	}
	s.writeProto(w, http.StatusOK, response)
}

func (s *Server) handleHosts(w http.ResponseWriter, r *http.Request, _ map[string]string) {
	identity, err := s.requireDevice(r)
	if err != nil {
		s.writeError(w, r, err)
		return
	}
	views, err := s.deps.Auth.ListHosts(r.Context(), identity)
	if err != nil {
		s.writeError(w, r, err)
		return
	}
	list := &whalereinv1.HostList{Hosts: make([]*whalereinv1.HostInfo, 0, len(views))}
	for _, view := range views {
		list.Hosts = append(list.Hosts, &whalereinv1.HostInfo{
			HostId:          view.HostID,
			Online:          view.Online,
			ProtocolVersion: view.ProtocolVersion,
			Fingerprint:     view.Fingerprint,
		})
	}
	s.writeProto(w, http.StatusOK, list)
}

func (s *Server) handleSubmit(w http.ResponseWriter, r *http.Request, params map[string]string) {
	identity, err := s.requireDevice(r)
	if err != nil {
		s.writeError(w, r, err)
		return
	}
	hostID := params["host_id"]
	if hostID == "" {
		s.writeError(w, r, errRouteNotFound)
		return
	}
	if err := s.deps.Auth.AuthorizeHost(r.Context(), identity, hostID); err != nil {
		s.writeError(w, r, err)
		return
	}
	var request whalereinv1.HostRequest
	if err := s.decodeProto(w, r, &request); err != nil {
		s.writeError(w, r, err)
		return
	}
	if request.GetHostId() != "" && request.GetHostId() != hostID {
		s.writeError(w, r, fmt.Errorf("%w: host_id differs from path", requests.ErrInvalidRequest))
		return
	}
	if request.GetDeviceId() != "" && request.GetDeviceId() != identity.DeviceID {
		s.writeError(w, r, requests.ErrForbidden)
		return
	}
	request.HostId = hostID
	request.DeviceId = identity.DeviceID
	if request.GetTraceId() == "" {
		if meta := requestMetaFrom(r.Context()); meta != nil {
			request.TraceId = meta.traceID
		}
	}
	row, _, err := s.deps.Requests.Submit(r.Context(), requests.SubmitInput{
		DeviceID: identity.DeviceID,
		HostID:   hostID,
		Request:  &request,
	})
	if err != nil {
		s.writeError(w, r, err)
		return
	}
	s.writeProto(w, http.StatusOK, &whalereinv1.RequestReceipt{
		RequestId:       row.RequestID,
		Status:          row.Status,
		CommandId:       row.CommandID,
		ExpiresAtUnixMs: row.ExpiresAt.UnixMilli(),
	})
}

func (s *Server) handleGetRequest(w http.ResponseWriter, r *http.Request, params map[string]string) {
	identity, err := s.requireDevice(r)
	if err != nil {
		s.writeError(w, r, err)
		return
	}
	row, err := s.deps.Requests.Get(r.Context(), identity.DeviceID, params["request_id"])
	if err != nil {
		s.writeError(w, r, err)
		return
	}
	s.deps.Requests.Touch(identity.DeviceID, row.RequestID, row.Status)
	s.writeProto(w, http.StatusOK, &whalereinv1.RequestState{
		RequestId:       row.RequestID,
		HostId:          row.HostID,
		DeviceId:        row.DeviceID,
		Status:          row.Status,
		CommandId:       row.CommandID,
		PayloadDigest:   row.PayloadDigest,
		ResultBody:      row.ResultBody,
		ErrorCode:       row.ErrorCode,
		ErrorMessage:    row.ErrorMessage,
		ExpiresAtUnixMs: row.ExpiresAt.UnixMilli(),
		UpdatedAtUnixMs: row.UpdatedAt.UnixMilli(),
		ChannelId:       row.ChannelID,
	})
}

func (s *Server) handleHeartbeat(w http.ResponseWriter, r *http.Request, params map[string]string) {
	identity, err := s.requireDevice(r)
	if err != nil {
		s.writeError(w, r, err)
		return
	}
	row, err := s.deps.Requests.Get(r.Context(), identity.DeviceID, params["request_id"])
	if err != nil {
		s.writeError(w, r, err)
		return
	}
	s.deps.Requests.Touch(identity.DeviceID, row.RequestID, row.Status)
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) handleDeleteDevice(w http.ResponseWriter, r *http.Request, params map[string]string) {
	deviceID := params["device_id"]
	if deviceID == "" {
		s.writeError(w, r, errRouteNotFound)
		return
	}
	token, err := bearerToken(r)
	if err != nil {
		s.writeError(w, r, err)
		return
	}
	if adminErr := s.deps.Auth.AuthenticateAdmin(token); adminErr != nil {
		identity, deviceErr := s.deps.Auth.AuthenticateDeviceToken(r.Context(), token)
		if deviceErr != nil {
			s.writeError(w, r, deviceErr)
			return
		}
		if identity.DeviceID != deviceID {
			s.writeError(w, r, auth.ErrForbidden)
			return
		}
	}
	if err := s.deps.Auth.RevokeDevice(r.Context(), deviceID); err != nil {
		s.writeError(w, r, err)
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) handleHealth(w http.ResponseWriter, r *http.Request, _ map[string]string) {
	diagnostics.HealthHandler().ServeHTTP(w, r)
}

func (s *Server) handleReady(w http.ResponseWriter, r *http.Request, _ map[string]string) {
	s.deps.Readiness.ReadyHandler().ServeHTTP(w, r)
}

func (s *Server) handleMetrics(w http.ResponseWriter, r *http.Request, _ map[string]string) {
	s.deps.Metrics.Handler().ServeHTTP(w, r)
}

func (s *Server) requireDevice(r *http.Request) (auth.Identity, error) {
	token, err := bearerToken(r)
	if err != nil {
		return auth.Identity{}, err
	}
	return s.deps.Auth.AuthenticateDeviceToken(r.Context(), token)
}

func bearerToken(r *http.Request) (string, error) {
	header := strings.TrimSpace(r.Header.Get("Authorization"))
	parts := strings.Fields(header)
	if len(parts) != 2 || !strings.EqualFold(parts[0], "Bearer") || parts[1] == "" {
		return "", errMissingAuth
	}
	return parts[1], nil
}

func (s *Server) decodeProto(w http.ResponseWriter, r *http.Request, message proto.Message) error {
	reader := http.MaxBytesReader(w, r.Body, s.deps.MaxBodyBytes)
	data, err := io.ReadAll(reader)
	if err != nil {
		var maxErr *http.MaxBytesError
		if errors.As(err, &maxErr) {
			return errBodyTooLarge
		}
		return errInvalidBody
	}
	if len(strings.TrimSpace(string(data))) == 0 {
		return errInvalidBody
	}
	if err := (protojson.UnmarshalOptions{DiscardUnknown: false}).Unmarshal(data, message); err != nil {
		return errInvalidBody
	}
	return nil
}

func (s *Server) writeProto(w http.ResponseWriter, status int, message proto.Message) {
	data, err := (protojson.MarshalOptions{EmitUnpopulated: false}).Marshal(message)
	if err != nil {
		s.deps.Logger.Error("http response encode failed")
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusInternalServerError)
		_, _ = w.Write([]byte(`{"code":"internal","message":"internal error"}`))
		return
	}
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_, _ = w.Write(data)
}

func (s *Server) writeError(w http.ResponseWriter, r *http.Request, err error) {
	info := classify(err)
	if info.status >= 500 && s.deps.Logger != nil {
		s.deps.Logger.Error("http handler error", "status", info.status, "error", err.Error())
	}
	if meta := requestMetaFrom(r.Context()); meta != nil && meta.errorCode == "" {
		meta.errorCode = codeName(info.code)
	}
	trace := ""
	requestID := ""
	if meta := requestMetaFrom(r.Context()); meta != nil {
		trace = meta.traceID
		requestID = meta.requestID
	}
	s.writeProto(w, info.status, &whalereinv1.ErrorResponse{
		Code:      info.code,
		Message:   info.message,
		TraceId:   trace,
		RequestId: requestID,
	})
}

// handleBrokerConnect answers one deferred Mosquitto CONNECT validation. The
// response is the versioned broker policy for this connection: it carries the
// principal, its complete local ACL set and the monotonic policy revision.
func (s *Server) handleBrokerConnect(w http.ResponseWriter, r *http.Request, _ map[string]string) {
	// Read at most the 64 KiB contract bound plus one byte so an oversized
	// request is detected without buffering the whole body. The response stays
	// the versioned text protocol, so a plain deny is written instead of an
	// HTTP 413.
	body, err := io.ReadAll(io.LimitReader(r.Body, (64<<10)+1))
	if err != nil || len(body) > 64<<10 {
		writeBrokerPolicy(w, []byte("whalerein/1\ndeny malformed_request\n"))
		return
	}
	form, err := url.ParseQuery(string(body))
	if err != nil {
		writeBrokerPolicy(w, []byte("whalerein/1\ndeny malformed_request\n"))
		return
	}
	clientID := form.Get("clientid")
	username := form.Get("username")
	password := form.Get("password")
	will := form.Get("will")
	identity, err := s.deps.Auth.AuthenticateBroker(r.Context(), clientID, username, password)
	if err != nil {
		s.deps.Logger.Debug("broker authentication denied")
		writeBrokerPolicy(w, []byte("whalerein/1\ndeny unauthorized\n"))
		return
	}
	connection, err := s.deps.Auth.BrokerPolicy(r.Context(), identity)
	if err != nil {
		s.deps.Logger.Warn("broker policy projection failed", "error", err.Error())
		writeBrokerPolicy(w, []byte("whalerein/1\ndeny unavailable\n"))
		return
	}
	if !connection.WillAllowed(will) {
		writeBrokerPolicy(w, []byte("whalerein/1\ndeny will_not_authorized\n"))
		return
	}
	writeBrokerPolicy(w, policy.Format(connection))
}

func writeBrokerPolicy(w http.ResponseWriter, body []byte) {
	w.Header().Set("Content-Type", "text/plain; charset=utf-8")
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write(body)
}
