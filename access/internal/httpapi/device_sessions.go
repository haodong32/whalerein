package httpapi

import (
	"context"
	"encoding/json"
	"net/http"

	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

type externalDeviceSessions interface {
	InstallDeviceSession(context.Context, string, *whalereinv1.ExternalAuthorization) error
	RevokeDeviceSession(context.Context, string, int64) error
	DeviceSessionPairingStatus(context.Context, string, string, string) (string, error)
}

func (s *Server) requireExternalDeviceSessionManager(w http.ResponseWriter, r *http.Request) (externalDeviceSessions, []byte) {
	service, ok := s.deps.External.(externalDeviceSessions)
	if !ok {
		s.writeError(w, r, errExternalNotConfigured)
		return nil, nil
	}
	if err := s.externalReadAuth(r); err != nil {
		s.writeError(w, r, err)
		return nil, nil
	}
	body, err := s.readJSONBody(w, r)
	if err != nil {
		s.writeError(w, r, err)
		return nil, nil
	}
	return service, body
}

func (s *Server) handleInstallExternalDeviceSession(w http.ResponseWriter, r *http.Request, params map[string]string) {
	service, body := s.requireExternalDeviceSessionManager(w, r)
	if service == nil {
		return
	}
	var authorization whalereinv1.ExternalAuthorization
	if decodeJSONMessage(body, &authorization) != nil {
		s.writeError(w, r, errInvalidBody)
		return
	}
	if err := service.InstallDeviceSession(r.Context(), params["session_id"], &authorization); err != nil {
		s.writeError(w, r, classifyExternal(err))
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) handleRevokeExternalDeviceSession(w http.ResponseWriter, r *http.Request, params map[string]string) {
	service, body := s.requireExternalDeviceSessionManager(w, r)
	if service == nil {
		return
	}
	var revoke struct {
		Revision int64 `json:"revision"`
	}
	if json.Unmarshal(body, &revoke) != nil {
		s.writeError(w, r, errInvalidBody)
		return
	}
	if err := service.RevokeDeviceSession(r.Context(), params["session_id"], revoke.Revision); err != nil {
		s.writeError(w, r, classifyExternal(err))
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) handleExternalPairingStatus(w http.ResponseWriter, r *http.Request, params map[string]string) {
	service, ok := s.deps.External.(externalDeviceSessions)
	if !ok {
		s.writeError(w, r, errExternalNotConfigured)
		return
	}
	token, err := bearerToken(r)
	if err != nil {
		s.writeError(w, r, err)
		return
	}
	hostID, operationID := params["host_id"], params["operation_id"]
	if !validResourceID(hostID) || !validResourceID(operationID) {
		s.writeError(w, r, errInvalidBody)
		return
	}
	state, err := service.DeviceSessionPairingStatus(r.Context(), token, hostID, operationID)
	if err != nil {
		s.writeError(w, r, classifyExternal(err))
		return
	}
	s.writeJSON(w, http.StatusOK, map[string]string{"state": state})
}
