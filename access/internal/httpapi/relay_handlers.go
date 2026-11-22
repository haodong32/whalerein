package httpapi

import (
	"context"
	"encoding/base64"
	"errors"
	"io"
	"net/http"
	"strconv"
	"strings"

	"github.com/whalerein/whalerein/access/internal/relay"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// Fixed relay directory headers from proto/v1/WIRE.md.
const (
	RelayEpochHeader         = "WhaleRein-Relay-Epoch"
	RelayHostSignedAtHeader  = "WhaleRein-Host-Signed-At-Unix-Ms"
	RelayHostSignatureHeader = "WhaleRein-Host-Signature"
	RelayOwnerNodeHeader     = "WhaleRein-Owner-Node"
	ObjectExpiresHeader      = "WhaleRein-Object-Expires-At-Unix-Ms"
)

// RelayService is the ciphertext directory boundary used by HTTP. Handlers
// stay thin: they decode the wire shape, pass the authenticated headers
// through, and stream at most the reserved length.
type RelayService interface {
	Reserve(ctx context.Context, request relay.ReserveRequest) (relay.Reservation, error)
	PrepareUpload(ctx context.Context, request relay.UploadRequest) (relay.UploadTarget, error)
	Upload(ctx context.Context, target relay.UploadTarget, body io.Reader) error
	Finish(ctx context.Context, request relay.FinishRequest) (relay.Object, error)
	Fetch(ctx context.Context, request relay.ObjectRequest) (relay.FetchResult, error)
	Delete(ctx context.Context, request relay.ObjectRequest) (relay.DeleteResult, error)
}

// disabledRelay keeps every route present when the operator did not enable
// temporary storage, so a caller sees unsupported_capability instead of 404.
type disabledRelay struct{}

func (disabledRelay) Reserve(context.Context, relay.ReserveRequest) (relay.Reservation, error) {
	return relay.Reservation{}, relay.ErrDisabled
}

func (disabledRelay) PrepareUpload(context.Context, relay.UploadRequest) (relay.UploadTarget, error) {
	return relay.UploadTarget{}, relay.ErrDisabled
}

func (disabledRelay) Upload(context.Context, relay.UploadTarget, io.Reader) error {
	return relay.ErrDisabled
}

func (disabledRelay) Finish(context.Context, relay.FinishRequest) (relay.Object, error) {
	return relay.Object{}, relay.ErrDisabled
}

func (disabledRelay) Fetch(context.Context, relay.ObjectRequest) (relay.FetchResult, error) {
	return relay.FetchResult{}, relay.ErrDisabled
}

func (disabledRelay) Delete(context.Context, relay.ObjectRequest) (relay.DeleteResult, error) {
	return relay.DeleteResult{}, relay.ErrDisabled
}

func (s *Server) handleRelayReserve(w http.ResponseWriter, r *http.Request, _ map[string]string) {
	var message whalereinv1.RelayObjectReserveRequest
	if err := s.decodeProto(w, r, &message); err != nil {
		s.writeError(w, r, err)
		return
	}
	token, _ := bearerToken(r)
	reservation, err := s.deps.Relay.Reserve(r.Context(), relay.ReserveRequest{
		RelayVersion:    message.GetRelayVersion(),
		HostID:          message.GetHostId(),
		DeviceID:        message.GetDeviceId(),
		SessionID:       message.GetSessionId(),
		ChannelID:       message.GetChannelId(),
		Purpose:         message.GetPurpose(),
		PartID:          message.GetPartId(),
		CiphertextBytes: message.GetCiphertextBytes(),
		SHA256:          message.GetSha256(),
		ExpiresAtUnixMs: message.GetExpiresAtUnixMs(),
		SignedAtUnixMs:  message.GetSignedAtUnixMs(),
		Signature:       message.GetSignature(),
		BearerToken:     token,
		OwnerNode:       r.Header.Get(RelayOwnerNodeHeader),
	})
	if err != nil {
		s.writeRelayError(w, r, err)
		return
	}
	s.writeProto(w, http.StatusOK, &whalereinv1.RelayObjectReservation{
		ObjectId:        reservation.ObjectID,
		OwnerNodeId:     reservation.OwnerNodeID,
		UploadPath:      reservation.UploadPath,
		MaxBytes:        reservation.MaxBytes,
		ExpiresAtUnixMs: reservation.ExpiresAtUnixMs,
		UploadTimeoutMs: reservation.UploadTimeoutMS,
	})
}

func (s *Server) handleRelayUpload(w http.ResponseWriter, r *http.Request, params map[string]string) {
	epoch, err := relayEpochHeader(r)
	if err != nil {
		s.writeRelayError(w, r, err)
		return
	}
	token, _ := bearerToken(r)
	target, err := s.deps.Relay.PrepareUpload(r.Context(), relay.UploadRequest{
		ObjectRequest: relay.ObjectRequest{
			ObjectID:       params["object_id"],
			RelayVersion:   epoch,
			BearerToken:    token,
			OwnerNode:      r.Header.Get(RelayOwnerNodeHeader),
			SignedAtUnixMs: relaySignedAtHeader(r),
			Signature:      relaySignatureHeader(r),
		},
		ContentLength:    r.ContentLength,
		HasContentLength: r.ContentLength >= 0,
	})
	if err != nil {
		s.writeRelayError(w, r, err)
		return
	}
	// The declared length is authoritative for this hop: one byte beyond the
	// reservation is enough to prove a lying body without buffering it.
	body := http.MaxBytesReader(w, r.Body, target.Length+1)
	if err := s.deps.Relay.Upload(r.Context(), target, body); err != nil {
		// A body longer than the reservation is a conflict, not an internal
		// error: the transport cap proves the caller lied about the length.
		var maxErr *http.MaxBytesError
		if errors.As(err, &maxErr) {
			err = relay.ErrConflict
		}
		s.writeRelayError(w, r, err)
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) handleRelayFinish(w http.ResponseWriter, r *http.Request, params map[string]string) {
	var message whalereinv1.RelayObjectFinishRequest
	if err := s.decodeProto(w, r, &message); err != nil {
		s.writeError(w, r, err)
		return
	}
	token, _ := bearerToken(r)
	object, err := s.deps.Relay.Finish(r.Context(), relay.FinishRequest{
		ObjectRequest: relay.ObjectRequest{
			ObjectID:       params["object_id"],
			RelayVersion:   message.GetRelayVersion(),
			BearerToken:    token,
			OwnerNode:      r.Header.Get(RelayOwnerNodeHeader),
			SignedAtUnixMs: message.GetSignedAtUnixMs(),
			Signature:      message.GetSignature(),
		},
		CiphertextBytes: message.GetCiphertextBytes(),
		SHA256:          message.GetSha256(),
	})
	if err != nil {
		s.writeRelayError(w, r, err)
		return
	}
	s.writeProto(w, http.StatusOK, &whalereinv1.RelayObject{
		ObjectId:        object.ObjectID,
		OwnerNodeId:     object.OwnerNodeID,
		Purpose:         object.Purpose,
		CiphertextBytes: object.CiphertextBytes,
		Sha256:          object.SHA256,
		ExpiresAtUnixMs: object.ExpiresAtUnixMs,
	})
}

func (s *Server) handleRelayFetch(w http.ResponseWriter, r *http.Request, params map[string]string) {
	epoch, err := relayEpochHeader(r)
	if err != nil {
		s.writeRelayError(w, r, err)
		return
	}
	token, _ := bearerToken(r)
	result, err := s.deps.Relay.Fetch(r.Context(), relay.ObjectRequest{
		ObjectID:       params["object_id"],
		RelayVersion:   epoch,
		BearerToken:    token,
		OwnerNode:      r.Header.Get(RelayOwnerNodeHeader),
		SignedAtUnixMs: relaySignedAtHeader(r),
		Signature:      relaySignatureHeader(r),
	})
	if err != nil {
		s.writeRelayError(w, r, err)
		return
	}
	defer func() { _ = result.Body.Close() }()
	w.Header().Set("Content-Type", "application/octet-stream")
	w.Header().Set("Content-Length", strconv.FormatInt(result.Length, 10))
	w.Header().Set("Digest", "sha-256="+base64.StdEncoding.EncodeToString(result.Digest))
	w.Header().Set(ObjectExpiresHeader, strconv.FormatInt(result.ExpiresAtUnixMs, 10))
	w.Header().Set("Cache-Control", "no-store")
	w.WriteHeader(http.StatusOK)
	if _, err := io.Copy(w, result.Body); err != nil {
		// The status and length are already committed. A partial stream is
		// never a complete object, and the caller must verify the digest.
		s.deps.Logger.Warn("relay object stream ended early", "trace_id", relayTraceID(r))
	}
}

func (s *Server) handleRelayDelete(w http.ResponseWriter, r *http.Request, params map[string]string) {
	epoch, err := relayEpochHeader(r)
	if err != nil {
		s.writeRelayError(w, r, err)
		return
	}
	token, _ := bearerToken(r)
	result, err := s.deps.Relay.Delete(r.Context(), relay.ObjectRequest{
		ObjectID:       params["object_id"],
		RelayVersion:   epoch,
		BearerToken:    token,
		OwnerNode:      r.Header.Get(RelayOwnerNodeHeader),
		SignedAtUnixMs: relaySignedAtHeader(r),
		Signature:      relaySignatureHeader(r),
	})
	if err != nil {
		s.writeRelayError(w, r, err)
		return
	}
	s.writeProto(w, http.StatusOK, &whalereinv1.RelayObjectDeleteResult{Cancelled: result.Cancelled})
}

// relayEpochHeader reads the required epoch header for PUT/GET/DELETE. Any
// missing, non-numeric or different value is a protocol incompatibility.
func relayEpochHeader(r *http.Request) (uint32, error) {
	raw := strings.TrimSpace(r.Header.Get(RelayEpochHeader))
	if raw == "" {
		return 0, relay.ErrEpoch
	}
	value, err := strconv.ParseUint(raw, 10, 32)
	if err != nil {
		return 0, relay.ErrEpoch
	}
	return uint32(value), nil
}

func relaySignedAtHeader(r *http.Request) int64 {
	raw := strings.TrimSpace(r.Header.Get(RelayHostSignedAtHeader))
	if raw == "" {
		return 0
	}
	value, err := strconv.ParseInt(raw, 10, 64)
	if err != nil {
		return 0
	}
	return value
}

// relaySignatureHeader decodes the base64url Host signature. An invalid value
// yields an empty slice, which the relay treats as a missing signature.
func relaySignatureHeader(r *http.Request) []byte {
	raw := strings.TrimSpace(r.Header.Get(RelayHostSignatureHeader))
	if raw == "" {
		return nil
	}
	decoded, err := base64.RawURLEncoding.DecodeString(raw)
	if err != nil {
		if padded, paddedErr := base64.URLEncoding.DecodeString(raw); paddedErr == nil {
			return padded
		}
		return nil
	}
	return decoded
}

// writeRelayError names the requested owner on a wrong-owner answer and then
// uses the shared wire error mapping.
func (s *Server) writeRelayError(w http.ResponseWriter, r *http.Request, err error) {
	var wrongOwner *relay.WrongOwnerError
	if errors.As(err, &wrongOwner) && wrongOwner.Requested != "" {
		w.Header().Set(RelayOwnerNodeHeader, wrongOwner.Requested)
	}
	s.writeError(w, r, err)
}

func relayTraceID(r *http.Request) string {
	if meta := requestMetaFrom(r.Context()); meta != nil {
		return meta.traceID
	}
	return ""
}
