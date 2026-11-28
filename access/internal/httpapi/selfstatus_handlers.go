package httpapi

import (
	"errors"
	"fmt"
	"io"
	"net/http"
	"time"

	"github.com/whalerein/whalerein/access/internal/auth"
	"github.com/whalerein/whalerein/access/internal/requests"
	"github.com/whalerein/whalerein/access/internal/selfstatus"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

// handleSelfStatus serves the bounded HTTPS bootstrap exchange used after the
// last ordinary grant closes. It requires no bearer token, session or ticket:
// the authenticated Host answers the status, and Access only relays.
func (s *Server) handleSelfStatus(w http.ResponseWriter, r *http.Request, params map[string]string) {
	// No answer on this route, including a bounded failure, is cacheable.
	w.Header().Set("Cache-Control", "no-store")
	if s.deps.SelfStatus == nil {
		s.writeError(w, r, errSelfStatusDisabled)
		return
	}
	hostID := params["host_id"]
	if !validResourceID(hostID) {
		s.writeError(w, r, auth.ErrBadRequest)
		return
	}
	limit := s.deps.SelfStatusMaxBytes
	if limit <= 0 || limit > selfstatus.MaxRequestBytes {
		limit = selfstatus.MaxRequestBytes
	}
	frame, err := s.decodeSelfStatusFrame(w, r, hostID, limit)
	if err != nil {
		s.writeError(w, r, err)
		return
	}
	caller := "unknown"
	trace := ""
	if meta := requestMetaFrom(r.Context()); meta != nil {
		trace = meta.traceID
		// A windowed pseudonym bounds per-caller admission without retaining or
		// logging the raw address.
		if reference, _ := s.deps.ClientReference.Reference(meta.client.Address, time.Now().UTC()); reference != "" {
			caller = reference
		}
	}
	status, err := s.deps.SelfStatus.Submit(r.Context(), selfstatus.Input{
		HostID:    hostID,
		GrantID:   frame.GetGrantId(),
		Challenge: frame.GetChallenge(),
		Caller:    caller,
		TraceID:   trace,
	})
	if err != nil {
		s.writeError(w, r, err)
		return
	}
	w.Header().Set("Cache-Control", "no-store")
	s.writeProto(w, http.StatusOK, status)
}

// decodeSelfStatusFrame accepts only a bounded HostRequest whose kind is
// SELF_STATUS and whose inline body is one SelfStatusRequest. Every routing,
// identity, reference and generation field must be absent: this endpoint is not
// an ordinary submission path and never forwards a caller-supplied envelope.
func (s *Server) decodeSelfStatusFrame(w http.ResponseWriter, r *http.Request, hostID string, limit int64) (*whalereinv1.SelfStatusRequest, error) {
	if err := requireJSONContentType(r); err != nil {
		return nil, err
	}
	data, err := io.ReadAll(http.MaxBytesReader(w, r.Body, limit))
	if err != nil {
		var maxErr *http.MaxBytesError
		if errors.As(err, &maxErr) {
			return nil, errBodyTooLarge
		}
		return nil, errInvalidBody
	}
	if len(data) == 0 {
		return nil, errInvalidBody
	}
	var envelope whalereinv1.HostRequest
	if err := decodeJSONMessage(data, &envelope); err != nil {
		return nil, err
	}
	if err := validateSelfStatusEnvelope(&envelope, hostID); err != nil {
		return nil, err
	}
	body := envelope.GetBody()
	if len(body) == 0 || len(body) > selfstatus.MaxFrameBytes {
		return nil, selfstatus.ErrInvalid
	}
	var frame whalereinv1.SelfStatusRequest
	if err := proto.Unmarshal(body, &frame); err != nil {
		return nil, errInvalidBody
	}
	if err := selfstatus.ValidateFrame(frame.GetGrantId(), frame.GetChallenge()); err != nil {
		return nil, err
	}
	return &frame, nil
}

func validateSelfStatusEnvelope(envelope *whalereinv1.HostRequest, hostID string) error {
	if envelope.GetKind() != whalereinv1.RequestKind_REQUEST_KIND_SELF_STATUS {
		return fmt.Errorf("%w: only SELF_STATUS is admitted", selfstatus.ErrInvalid)
	}
	if envelope.GetHostId() != "" && envelope.GetHostId() != hostID {
		return fmt.Errorf("%w: host claim differs from path", selfstatus.ErrInvalid)
	}
	if envelope.GetRequestId() != "" || envelope.GetDeviceId() != "" ||
		envelope.GetCommandId() != "" || envelope.GetChannelId() != "" ||
		len(envelope.GetPayloadDigest()) != 0 || envelope.GetBodyReference() != nil ||
		envelope.GetTransport() != nil || envelope.GetConnectionRef() != "" ||
		envelope.GetGrantGeneration() != 0 || envelope.GetEnrollmentGeneration() != 0 {
		return fmt.Errorf("%w: ordinary request fields are not admitted", selfstatus.ErrInvalid)
	}
	if envelope.GetProtocolVersion() != 0 && envelope.GetProtocolVersion() != 1 {
		return fmt.Errorf("%w: protocol version", selfstatus.ErrInvalid)
	}
	if envelope.GetRelayVersion() != 0 && envelope.GetRelayVersion() != requests.RelayEpoch {
		return fmt.Errorf("%w: relay epoch", selfstatus.ErrInvalid)
	}
	return nil
}
