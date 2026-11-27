package httpapi

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"net/http"

	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

type sessionProofBody struct {
	HostID          string `json:"hostId"`
	DeviceID        string `json:"deviceId"`
	ChannelID       string `json:"channelId"`
	TranscriptHash  string `json:"transcriptHash"`
	IssuedAtUnixMs  int64  `json:"issuedAtUnixMs"`
	ExpiresAtUnixMs int64  `json:"expiresAtUnixMs"`
	Signature       string `json:"signature"`
}

func (s *Server) handleSessionProof(w http.ResponseWriter, r *http.Request, params map[string]string) {
	service, ok := s.deps.External.(interface {
		RegisterSessionProof(context.Context, string, string, *whalereinv1.SessionProof) error
	})
	if !ok {
		s.writeError(w, r, errExternalNotConfigured)
		return
	}
	credential, err := bearerToken(r)
	if err != nil {
		s.writeError(w, r, err)
		return
	}
	var body sessionProofBody
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 16*1024)).Decode(&body); err != nil {
		s.writeError(w, r, errInvalidBody)
		return
	}
	hash, hashErr := base64.RawURLEncoding.DecodeString(body.TranscriptHash)
	signature, signatureErr := base64.RawURLEncoding.DecodeString(body.Signature)
	if hashErr != nil || signatureErr != nil || len(hash) != 32 || len(signature) != 64 {
		s.writeError(w, r, errInvalidBody)
		return
	}
	proof := &whalereinv1.SessionProof{
		SessionId: params["session_id"], HostId: body.HostID, DeviceId: body.DeviceID,
		ChannelId: body.ChannelID, TranscriptHash: hash,
		IssuedAtUnixMs: body.IssuedAtUnixMs, ExpiresAtUnixMs: body.ExpiresAtUnixMs,
		Signature: signature,
	}
	err = service.RegisterSessionProof(r.Context(), params["session_id"], credential, proof)
	if err != nil {
		s.writeError(w, r, err)
		return
	}
	s.writeJSON(w, http.StatusOK, map[string]string{"state": "accepted"})
}
