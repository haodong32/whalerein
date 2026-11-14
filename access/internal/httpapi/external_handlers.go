package httpapi

import (
	"errors"
	"net/http"

	"github.com/whalerein/whalerein/access/internal/external"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// Stable errors for the external account endpoints.
var (
	errAuthorizationExpired  = errors.New("authorization expired")
	errAuthorizationRefused  = errors.New("authorization refused")
	errOperationConflict     = errors.New("operation conflict")
	errExternalNotConfigured = errors.New("external account mode is not configured")
)

// handleEnrollHost serves O3: it verifies a portable authorization and the
// Host's proof of possession before committing one enrollment.
func (s *Server) handleEnrollHost(w http.ResponseWriter, r *http.Request, _ map[string]string) {
	if s.deps.External == nil {
		s.writeError(w, r, errExternalNotConfigured)
		return
	}
	var request whalereinv1.HostEnrollmentRequest
	if err := s.decodeProto(w, r, &request); err != nil {
		s.writeError(w, r, err)
		return
	}
	response, err := s.deps.External.EnrollHost(r.Context(), &request)
	if err != nil {
		s.writeError(w, r, classifyExternal(err))
		return
	}
	s.writeProto(w, http.StatusOK, response)
}

// handleEnrollmentStatus serves O4 and never returns a credential.
func (s *Server) handleEnrollmentStatus(w http.ResponseWriter, r *http.Request, params map[string]string) {
	if s.deps.External == nil {
		s.writeError(w, r, errExternalNotConfigured)
		return
	}
	if err := s.externalReadAuth(r); err != nil {
		s.writeError(w, r, err)
		return
	}
	status, err := s.deps.External.EnrollmentStatus(r.Context(), params["host_id"])
	if err != nil {
		s.writeError(w, r, classifyExternal(err))
		return
	}
	s.writeProto(w, http.StatusOK, status)
}

// handleOperationStatus serves O6 and never returns a credential.
func (s *Server) handleOperationStatus(w http.ResponseWriter, r *http.Request, params map[string]string) {
	if s.deps.External == nil {
		s.writeError(w, r, errExternalNotConfigured)
		return
	}
	if err := s.externalReadAuth(r); err != nil {
		s.writeError(w, r, err)
		return
	}
	status, err := s.deps.External.OperationStatus(r.Context(), params["operation_id"])
	if err != nil {
		s.writeError(w, r, classifyExternal(err))
		return
	}
	s.writeProto(w, http.StatusOK, status)
}

// externalReadAuth accepts the admin bearer for status reads.
//
// The contract also admits a Device bearer holding a grant to that Host, or
// the Host's own proof. Those credentials arrive with the device and Host
// flows in tasks 08 and 10, so this build states the narrower rule instead of
// implying the wider one.
func (s *Server) externalReadAuth(r *http.Request) error {
	token, err := bearerToken(r)
	if err != nil {
		return err
	}
	return s.deps.Auth.AuthenticateAdmin(token)
}

// classifyExternal maps a refusal to a stable HTTP error.
func classifyExternal(err error) error {
	switch {
	case errors.Is(err, external.ErrAuthorizationExpired):
		return errAuthorizationExpired
	case errors.Is(err, external.ErrAuthorizationMismatch),
		errors.Is(err, external.ErrAuthorizationSignature),
		errors.Is(err, external.ErrProofInvalid),
		errors.Is(err, external.ErrKeyNotValid),
		errors.Is(err, external.ErrEnrollmentDisabled):
		return errAuthorizationRefused
	case errors.Is(err, external.ErrOperationConflict),
		errors.Is(err, store.ErrOperationConflict),
		errors.Is(err, external.ErrStaleRevision):
		return errOperationConflict
	case errors.Is(err, store.ErrNotFound), errors.Is(err, external.ErrNotFound):
		return errRouteNotFound
	default:
		return err
	}
}
