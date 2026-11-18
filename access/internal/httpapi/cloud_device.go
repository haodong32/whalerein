package httpapi

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"

	"github.com/whalerein/whalerein/access/internal/auth"
	"github.com/whalerein/whalerein/access/internal/external"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

type externalDeviceService interface {
	PairDevice(context.Context, *whalereinv1.ExternalAuthorization, *whalereinv1.PairingClaimRequest) (*whalereinv1.PairingClaimResponse, error)
	DeviceGrantStatus(context.Context, string, string, string) (string, error)
	RevokeDeviceGrant(context.Context, string, string, string) error
}

func (s *Server) handleCloudPairing(w http.ResponseWriter, r *http.Request, _ map[string]string) {
	service, ok := s.deps.External.(externalDeviceService)
	if !ok {
		s.writeError(w, r, errExternalNotConfigured)
		return
	}
	body, err := s.readJSONBody(w, r)
	if err != nil {
		s.writeError(w, r, err)
		return
	}
	var envelope map[string]json.RawMessage
	if err := json.Unmarshal(body, &envelope); err != nil || len(envelope) != 2 {
		s.writeError(w, r, errInvalidBody)
		return
	}
	var authorization whalereinv1.ExternalAuthorization
	var claim whalereinv1.PairingClaimRequest
	if decodeJSONMessage(envelope["authorization"], &authorization) != nil ||
		decodeJSONMessage(envelope["claim"], &claim) != nil {
		s.writeError(w, r, errInvalidBody)
		return
	}
	ctx := r.Context()
	if token, err := bearerToken(r); err == nil {
		ctx = external.WithDeviceSessionCredential(ctx, token)
	}
	response, err := service.PairDevice(ctx, &authorization, &claim)
	if err != nil {
		s.writeError(w, r, classifyExternal(err))
		return
	}
	s.writeProto(w, http.StatusOK, response)
}

func (s *Server) cloudDeviceScope(w http.ResponseWriter, r *http.Request, params map[string]string) (externalDeviceService, string) {
	service, ok := s.deps.External.(externalDeviceService)
	if !ok {
		s.writeError(w, r, errExternalNotConfigured)
		return nil, ""
	}
	if err := s.externalReadAuth(r); err != nil {
		s.writeError(w, r, err)
		return nil, ""
	}
	workspace := r.URL.Query()["workspaceId"]
	if len(workspace) != 1 || !validResourceID(workspace[0]) ||
		!validResourceID(params["host_id"]) || !validResourceID(params["device_id"]) {
		s.writeError(w, r, auth.ErrBadRequest)
		return nil, ""
	}
	return service, workspace[0]
}

func (s *Server) handleCloudDeviceStatus(w http.ResponseWriter, r *http.Request, params map[string]string) {
	service, workspace := s.cloudDeviceScope(w, r, params)
	if service == nil {
		return
	}
	state, err := service.DeviceGrantStatus(r.Context(), workspace, params["host_id"], params["device_id"])
	if errors.Is(err, store.ErrCloudGrantMissing) {
		s.writeJSON(w, http.StatusNotFound, map[string]string{"state": "missing_grant"})
		return
	}
	if err != nil {
		s.writeError(w, r, classifyExternal(err))
		return
	}
	s.writeJSON(w, http.StatusOK, map[string]string{"state": state})
}

func (s *Server) handleCloudDeviceRevocation(w http.ResponseWriter, r *http.Request, params map[string]string) {
	service, workspace := s.cloudDeviceScope(w, r, params)
	if service == nil {
		return
	}
	if err := service.RevokeDeviceGrant(r.Context(), workspace, params["host_id"], params["device_id"]); err != nil {
		s.writeError(w, r, classifyExternal(err))
		return
	}
	w.WriteHeader(http.StatusNoContent)
}
